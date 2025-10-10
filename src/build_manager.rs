use crate::config::Config;
use crate::queue::{Job, JobQueue, JobStatus};
use crate::runner;
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

pub type PrNumber = u64;

#[derive(Debug)]
pub struct ActiveBuild {
    pub job: Job,
    pub task_handle: JoinHandle<()>,
    pub cancellation_token: CancellationToken,
}

impl ActiveBuild {
    pub fn cancel(&self) {
        info!("Cancelling active build for PR #{}", self.job.pr_number);
        self.cancellation_token.cancel();
        self.task_handle.abort();
    }
}

pub enum BuildManagerMessage {
    SubmitJob(Job),
    CancelPrBuilds(PrNumber),
    GetQueueStatus(oneshot::Sender<QueueStatus>),
    BuildFinished(PrNumber),
}

#[derive(Debug, Clone)]
pub struct QueueStatus {
    pub queue_length: usize,
    pub active_builds: Vec<PrNumber>,
}

pub struct BuildManager {
    message_tx: mpsc::UnboundedSender<BuildManagerMessage>,
    _worker_handle: JoinHandle<()>,
}

impl BuildManager {
    pub fn new(
        config: Arc<Config>,
        github_client: Arc<octocrab::Octocrab>,
        max_queue_size: usize,
        max_concurrency: usize,
    ) -> Result<Self> {
        let (message_tx, message_rx) = mpsc::unbounded_channel();

        let worker = BuildManagerWorker::new(
            config,
            github_client,
            max_queue_size,
            max_concurrency,
            message_tx.clone(),
            message_rx,
        );
        let worker_handle = tokio::spawn(async move {
            if let Err(e) = worker.run().await {
                error!("Build manager worker failed: {}", e);
            }
        });

        Ok(Self {
            message_tx,
            _worker_handle: worker_handle,
        })
    }

    pub async fn submit_job(&self, job: Job) -> Result<()> {
        self.message_tx
            .send(BuildManagerMessage::SubmitJob(job))
            .map_err(|_| anyhow!("Build manager channel closed"))?;
        Ok(())
    }

    pub async fn cancel_pr_builds(&self, pr_number: PrNumber) -> Result<()> {
        self.message_tx
            .send(BuildManagerMessage::CancelPrBuilds(pr_number))
            .map_err(|_| anyhow!("Build manager channel closed"))?;
        Ok(())
    }

    pub async fn get_queue_status(&self) -> Result<QueueStatus> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.message_tx
            .send(BuildManagerMessage::GetQueueStatus(tx))
            .map_err(|_| anyhow!("Build manager channel closed"))?;

        rx.await.map_err(|_| anyhow!("Failed to get queue status"))
    }
}

struct BuildManagerWorker {
    config: Arc<Config>,
    github_client: Arc<octocrab::Octocrab>,
    queue: JobQueue,
    active_builds: HashMap<PrNumber, ActiveBuild>,
    message_tx: mpsc::UnboundedSender<BuildManagerMessage>,
    message_rx: mpsc::UnboundedReceiver<BuildManagerMessage>,
    semaphore: Arc<Semaphore>,
    max_concurrency: usize,
}

impl BuildManagerWorker {
    fn new(
        config: Arc<Config>,
        github_client: Arc<octocrab::Octocrab>,
        max_queue_size: usize,
        max_concurrency: usize,
        message_tx: mpsc::UnboundedSender<BuildManagerMessage>,
        message_rx: mpsc::UnboundedReceiver<BuildManagerMessage>,
    ) -> Self {
        Self {
            config,
            github_client,
            queue: JobQueue::new(max_queue_size),
            active_builds: HashMap::new(),
            message_tx,
            message_rx,
            semaphore: Arc::new(Semaphore::new(max_concurrency.max(1))),
            max_concurrency: max_concurrency.max(1),
        }
    }

    async fn run(mut self) -> Result<()> {
        info!("Build manager worker started");
        loop {
            tokio::select! {
                msg = self.message_rx.recv() => {
                    match msg {
                        Some(BuildManagerMessage::SubmitJob(job)) => {
                            self.handle_submit_job(job).await;
                        }
                        Some(BuildManagerMessage::CancelPrBuilds(pr_number)) => {
                            self.handle_cancel_pr_builds(pr_number).await;
                        }
                        Some(BuildManagerMessage::GetQueueStatus(tx)) => {
                            let status = QueueStatus {
                                queue_length: self.queue.len(),
                                active_builds: self.active_builds.keys().copied().collect(),
                            };
                            let _ = tx.send(status);
                        }
                        Some(BuildManagerMessage::BuildFinished(pr_number)) => {
                            self.handle_build_finished(pr_number).await;
                        }
                        None => {
                            info!("Build manager channel closed, shutting down");
                            break;
                        }
                    }
                }
            }
        }

        info!("Build manager worker shutting down");
        self.cleanup_all_builds().await;
        Ok(())
    }

    async fn handle_submit_job(&mut self, job: Job) {
        debug!(
            "Received job submission for PR #{}: {}",
            job.pr_number, job.id
        );

        if let Err(e) = self.queue.enqueue(job.clone()) {
            warn!("Failed to enqueue job {}: {}", job.id, e);
            return;
        }

        debug!("Job {} enqueued successfully", job.id);
        self.try_start_next_build().await;
    }

    async fn handle_cancel_pr_builds(&mut self, pr_number: PrNumber) {
        info!("Cancelling builds for PR #{}", pr_number);

        if let Some(active_build) = self.active_builds.remove(&pr_number) {
            active_build.cancel();
            debug!("Cancelled active build for PR #{}", pr_number);
        }

        let removed_jobs = self.queue.remove_pr_jobs(pr_number);
        if !removed_jobs.is_empty() {
            debug!(
                "Removed {} queued jobs for PR #{}",
                removed_jobs.len(),
                pr_number
            );
        }
    }

    async fn try_start_next_build(&mut self) {
        // Start up to max_concurrency builds using available permits
        loop {
            if self.active_builds.len() >= self.max_concurrency {
                break;
            }
            let permit = match self.semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => break,
            };
            let mut job = match self.queue.dequeue() {
                Some(j) => j,
                None => break,
            };
            debug!(id=%job.id, pr_number=%job.pr_number, "Starting build for job");
            job.status = JobStatus::Running;

            let cancellation_token = CancellationToken::new();
            let task_handle = self
                .spawn_build_task(job.clone(), cancellation_token.clone(), permit)
                .await;

            let active_build = ActiveBuild {
                job: job.clone(),
                task_handle,
                cancellation_token,
            };

            self.active_builds.insert(job.pr_number, active_build);
        }
    }

    async fn spawn_build_task(
        &self,
        job: Job,
        cancellation_token: CancellationToken,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> JoinHandle<()> {
        let config = Arc::clone(&self.config);
        let github_client = Arc::clone(&self.github_client);
        let message_tx = self.message_tx.clone();

        tokio::spawn(async move {
            // Hold the permit for the lifetime of the task
            let _permit = permit;
            let result = tokio::select! {
                result = runner::handle_pr_with_cancellation(
                    &config,
                    &github_client,
                    &job.repo_full_name,
                    &job.repo_clone_url,
                    &job.commit_sha,
                    job.pr_number,
                    cancellation_token.clone()
                ) => result,
                _ = cancellation_token.cancelled() => {
                    warn!("Build for PR #{} was cancelled", job.pr_number);
                    // notify finished to clean up state and possibly start next builds
                    let _ = message_tx.send(BuildManagerMessage::BuildFinished(job.pr_number));
                    return;
                }
            };

            if let Err(e) = result {
                error!("Build failed for PR #{}: {}", job.pr_number, e);
            } else {
                info!("Build completed successfully for PR #{}", job.pr_number);
            }

            // notify finished to clean up state and possibly start next builds
            let _ = message_tx.send(BuildManagerMessage::BuildFinished(job.pr_number));
        })
    }

    async fn handle_build_finished(&mut self, pr_number: PrNumber) {
        if let Some(active_build) = self.active_builds.remove(&pr_number) {
            debug!("Cleaning up finished build for PR #{}", pr_number);
            let _ = active_build.task_handle.await;
        } else {
            trace!(pr_number, "Received BuildFinished for non-active PR");
        }
        self.try_start_next_build().await;
    }

    async fn cleanup_all_builds(&mut self) {
        info!("Cleaning up all active builds");

        for (pr_number, active_build) in self.active_builds.drain() {
            debug!("Cancelling build for PR #{}", pr_number);
            active_build.cancel();
            let _ = active_build.task_handle.await;
        }
    }
}
