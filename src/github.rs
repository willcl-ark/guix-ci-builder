use anyhow::{Context, Result, anyhow};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

pub async fn upsert_pr_comment(
    github_client: &Arc<octocrab::Octocrab>,
    repo_full_name: &str,
    pr_number: u64,
    comment_body: &str,
) -> Result<()> {
    let (owner, repo) = repo_full_name
        .split_once('/')
        .ok_or_else(|| anyhow!("Invalid repository name format: {}", repo_full_name))?;

    let me = github_client
        .current()
        .user()
        .await
        .with_context(|| "fetch authenticated user".to_string())?;
    let my_login = me.login;

    debug!(pr_number, owner, repo, login = %my_login, "Searching for existing PR comment to update");
    let page = github_client
        .issues(owner, repo)
        .list_comments(pr_number)
        .per_page(100)
        .send()
        .await
        .with_context(|| format!("list comments for PR #{}", pr_number))?;

    // Prefer the most recent matching comment
    let existing = page
        .items
        .iter()
        .rev()
        .find(|c| c.user.login == my_login)
        .cloned();

    const MAX_ATTEMPTS: u32 = 3;
    if let Some(comment) = existing {
        debug!(pr_number, comment_id = %comment.id, "Updating existing PR comment");
        for attempt in 0..MAX_ATTEMPTS {
            match github_client
                .issues(owner, repo)
                .update_comment(comment.id, comment_body)
                .await
            {
                Ok(_) => {
                    info!(pr_number, comment_id = %comment.id, "Updated PR comment");
                    return Ok(());
                }
                Err(e) if attempt + 1 < MAX_ATTEMPTS => {
                    let backoff = 1u64 << attempt;
                    warn!(attempt, backoff, error = %e, pr_number, comment_id = %comment.id, "Failed to update comment, retrying");
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                }
                Err(e) => {
                    return Err(anyhow!(
                        "Failed to update PR comment after {} attempts: {}",
                        MAX_ATTEMPTS,
                        e
                    ));
                }
            }
        }
    } else {
        debug!(pr_number, owner, repo, "Creating new PR comment");
        for attempt in 0..MAX_ATTEMPTS {
            match github_client
                .issues(owner, repo)
                .create_comment(pr_number, comment_body)
                .await
            {
                Ok(_) => {
                    info!(pr_number, "Created PR comment");
                    return Ok(());
                }
                Err(e) if attempt + 1 < MAX_ATTEMPTS => {
                    let backoff = 1u64 << attempt;
                    warn!(attempt, backoff, error = %e, pr_number, "Failed to create comment, retrying");
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                }
                Err(e) => {
                    return Err(anyhow!(
                        "Failed to create PR comment after {} attempts: {}",
                        MAX_ATTEMPTS,
                        e
                    ));
                }
            }
        }
    }

    Err(anyhow!("Unexpected comment upsert loop exit"))
}
