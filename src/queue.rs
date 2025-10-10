use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tracing::{debug, info};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JobStatus {
    Pending,
    Running,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub pr_number: u64,
    pub commit_sha: String,
    pub repo_full_name: String,
    pub repo_clone_url: String,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
}

impl Job {
    pub fn new(
        pr_number: u64,
        commit_sha: String,
        repo_full_name: String,
        repo_clone_url: String,
    ) -> Self {
        let short_sha = commit_sha.get(..8).unwrap_or(commit_sha.as_str());
        let id = format!("pr-{}-{}", pr_number, short_sha);
        Self {
            id,
            pr_number,
            commit_sha,
            repo_full_name,
            repo_clone_url,
            status: JobStatus::Pending,
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug)]
pub struct JobQueue {
    queue: VecDeque<Job>,
    max_size: usize,
}

impl JobQueue {
    pub fn new(max_size: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            max_size,
        }
    }

    pub fn enqueue(&mut self, job: Job) -> Result<()> {
        if self.queue.len() >= self.max_size {
            return Err(anyhow!("Queue is full (max size: {})", self.max_size));
        }

        debug!("Enqueuing job: {} for PR #{}", job.id, job.pr_number);
        self.queue.push_back(job);
        debug!("Queue size: {}", self.queue.len());
        Ok(())
    }

    pub fn dequeue(&mut self) -> Option<Job> {
        let job = self.queue.pop_front();
        if let Some(ref job) = job {
            debug!("Dequeuing job: {} for PR #{}", job.id, job.pr_number);
            debug!("Queue size: {}", self.queue.len());
        }
        job
    }

    pub fn remove_pr_jobs(&mut self, pr_number: u64) -> Vec<Job> {
        let mut removed = Vec::new();
        self.queue.retain(|j| {
            if j.pr_number == pr_number {
                info!("Removing queued job: {} for PR #{}", j.id, j.pr_number);
                removed.push(j.clone());
                false
            } else {
                true
            }
        });
        debug!("Queue size after removal: {}", self.queue.len());
        removed
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_creation() {
        let job = Job::new(
            123,
            "abcdef1234567890".to_string(),
            "owner/repo".to_string(),
            "https://github.com/owner/repo.git".to_string(),
        );

        assert_eq!(job.pr_number, 123);
        assert_eq!(job.commit_sha, "abcdef1234567890");
        assert_eq!(job.id, "pr-123-abcdef12");
        assert_eq!(job.status, JobStatus::Pending);
    }

    #[test]
    fn test_queue_operations() {
        let mut queue = JobQueue::new(10);

        let job1 = Job::new(
            123,
            "abc123".to_string(),
            "owner/repo".to_string(),
            "url".to_string(),
        );
        let job2 = Job::new(
            456,
            "def456".to_string(),
            "owner/repo".to_string(),
            "url".to_string(),
        );

        queue.enqueue(job1.clone()).unwrap();
        queue.enqueue(job2.clone()).unwrap();

        assert_eq!(queue.len(), 2);

        let dequeued = queue.dequeue().unwrap();
        assert_eq!(dequeued.pr_number, 123);
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn test_remove_pr_jobs() {
        let mut queue = JobQueue::new(10);

        let job1 = Job::new(
            123,
            "abc".to_string(),
            "owner/repo".to_string(),
            "url".to_string(),
        );
        let job2 = Job::new(
            123,
            "def".to_string(),
            "owner/repo".to_string(),
            "url".to_string(),
        );
        let job3 = Job::new(
            456,
            "ghi".to_string(),
            "owner/repo".to_string(),
            "url".to_string(),
        );

        queue.enqueue(job1).unwrap();
        queue.enqueue(job2).unwrap();
        queue.enqueue(job3).unwrap();

        let removed = queue.remove_pr_jobs(123);
        assert_eq!(removed.len(), 2);
        assert_eq!(queue.len(), 1);

        let remaining = queue.dequeue().unwrap();
        assert_eq!(remaining.pr_number, 456);
    }

    #[test]
    fn test_job_short_sha() {
        let job = Job::new(
            7,
            "abcd".to_string(),
            "owner/repo".to_string(),
            "url".to_string(),
        );
        assert_eq!(job.id, "pr-7-abcd");
    }
}
