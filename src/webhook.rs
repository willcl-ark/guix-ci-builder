use anyhow::{Context, Result, anyhow};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::build_manager::BuildManager;
use crate::config::Config;
use crate::queue::Job;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Deserialize)]
pub struct WebhookPayload {
    pub action: Option<String>, // Optional for pings
    #[allow(dead_code)]
    pub number: Option<u64>,
    pub pull_request: Option<PullRequest>,
    pub repository: Repository,
    pub zen: Option<String>, // For pings
}

#[derive(Debug, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub head: GitRef,
    pub base: GitRef,
    #[allow(dead_code)]
    pub html_url: String,
}

#[derive(Debug, Deserialize)]
pub struct GitRef {
    pub sha: String,
    #[serde(rename = "ref")]
    #[allow(dead_code)]
    pub git_ref: String,
    #[allow(dead_code)]
    pub repo: Repository,
}

#[derive(Debug, Deserialize)]
pub struct Repository {
    #[allow(dead_code)]
    pub name: String,
    pub full_name: String,
    #[allow(dead_code)]
    pub clone_url: String,
    #[allow(dead_code)]
    pub html_url: String,
}

pub async fn handle_webhook(
    payload: &str,
    signature: Option<&str>,
    config: &Arc<Config>,
    build_manager: &Arc<BuildManager>,
) -> Result<()> {
    verify_signature(payload, signature, &config.webhook_secret)?;
    let webhook: WebhookPayload =
        serde_json::from_str(payload).with_context(|| "parse webhook payload".to_string())?;

    // Handle pings
    if let Some(zen) = &webhook.zen {
        info!("Received GitHub ping: {}", zen);
        info!(
            "Webhook configured successfully for repo: {}",
            webhook.repository.full_name
        );
        return Ok(());
    }

    let action = webhook.action.as_deref().unwrap_or("unknown");
    debug!(action, repo=%webhook.repository.full_name, "Received webhook");

    // We only handle pull request synchronize and opened events for now
    if action != "synchronize" && action != "opened" {
        debug!(action, "Ignoring webhook action");
        return Ok(());
    }

    let pr = webhook
        .pull_request
        .ok_or_else(|| anyhow!("Missing pull_request in webhook payload"))?;

    info!(pr_number=%pr.number, action, head=%pr.head.sha, base=%pr.base.sha, "Processing PR event");

    let repo_full_name = webhook.repository.full_name.clone();
    let repo_clone_url = webhook.repository.clone_url.clone();
    let commit_sha = pr.head.sha.clone();
    let pr_number = pr.number;

    // Cancel any existing builds for this PR before starting a new one
    info!(
        "Cancelling any existing builds for PR #{} before starting new build",
        pr_number
    );
    if let Err(e) = build_manager.cancel_pr_builds(pr_number).await {
        warn!(
            "Failed to cancel existing builds for PR #{}: {}",
            pr_number, e
        );
    }

    let job = Job::new(pr_number, commit_sha, repo_full_name, repo_clone_url);
    debug!(id=%job.id, pr_number, "Submitting job to build queue");
    if let Err(e) = build_manager.submit_job(job).await {
        warn!(pr_number, error=%e, "Failed to submit job");
        return Err(anyhow!("Failed to submit build job: {}", e));
    }
    debug!(pr_number, "Job successfully submitted");
    Ok(())
}

fn verify_signature(payload: &str, signature: Option<&str>, secret: &str) -> Result<()> {
    let signature_header =
        signature.ok_or_else(|| anyhow!("Missing X-Hub-Signature-256 header"))?;

    if !signature_header.starts_with("sha256=") {
        return Err(anyhow!("Invalid signature format"));
    }

    let signature_hex = &signature_header[7..]; // Remove "sha256=" prefix
    let expected_signature =
        hex::decode(signature_hex).map_err(|_| anyhow!("Invalid signature hex encoding"))?;

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| anyhow!("Invalid HMAC key"))?;
    mac.update(payload.as_bytes());

    mac.verify_slice(&expected_signature)
        .map_err(|_| anyhow!("Signature verification failed"))?;

    debug!("Webhook signature verified successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_signature_happy_path() {
        let payload = r#"{\"zen\":\"Nix fixes this.\"}"#;
        let secret = "topsecret";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload.as_bytes());
        let expected = mac.finalize().into_bytes();
        let header = format!("sha256={}", hex::encode(expected));

        let res = verify_signature(payload, Some(&header), secret);
        assert!(res.is_ok());
    }

    #[test]
    fn verify_signature_missing_header() {
        let payload = "{}";
        let secret = "topsecret";
        let res = verify_signature(payload, None, secret);
        assert!(res.is_err());
    }

    #[test]
    fn verify_signature_bad_format() {
        let payload = "{}";
        let secret = "topsecret";
        let res = verify_signature(payload, Some("notsha256=abcd"), secret);
        assert!(res.is_err());
    }
}
