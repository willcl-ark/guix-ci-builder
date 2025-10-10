use anyhow::{Context, Result, anyhow};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use tokio::fs;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::github;

async fn run_git(repo_dir: &Path, args: &[&str], what: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(args)
        .output()
        .await
        .with_context(|| format!("git {} (cwd: {})", what, repo_dir.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git {} failed: {}", what, stderr));
    }
    Ok(())
}

async fn fetch_origin(repo_dir: &Path) -> Result<()> {
    run_git(repo_dir, &["fetch", "origin"], "fetch origin").await
}

async fn fetch_pr_ref(repo_dir: &Path, pr_number: u64) -> Result<()> {
    let refspec = format!(
        "+refs/pull/{}/head:refs/remotes/origin/pr/{}/head",
        pr_number, pr_number
    );
    run_git(repo_dir, &["fetch", "origin", &refspec], "fetch PR ref").await
}

pub async fn handle_pr_with_cancellation(
    config: &Arc<Config>,
    github_client: &Arc<octocrab::Octocrab>,
    repo_full_name: &str,
    repo_clone_url: &str,
    commit_sha: &str,
    pr_number: u64,
    cancellation_token: CancellationToken,
) -> Result<()> {
    tokio::select! {
        result = handle_pr(config, github_client, repo_full_name, repo_clone_url, commit_sha, pr_number) => result,
        _ = cancellation_token.cancelled() => {
            debug!("Build for PR #{} was cancelled", pr_number);
            Err(anyhow!("Build was cancelled"))
        }
    }
}

pub async fn handle_pr(
    config: &Arc<Config>,
    github_client: &Arc<octocrab::Octocrab>,
    repo_full_name: &str,
    repo_clone_url: &str,
    commit_sha: &str,
    pr_number: u64,
) -> Result<()> {
    info!(
        "Starting build for PR #{} at commit {}",
        pr_number, commit_sha
    );

    // Create absolute paths by canonicalizing existing parts and joining non-existent parts
    let workspace_dir = if config.workspace_path.exists() {
        std::fs::canonicalize(&config.workspace_path)
            .unwrap_or_else(|_| config.workspace_path.clone())
    } else {
        // Canonicalize parent that exists, then join the workspace dir name
        let current_dir = std::env::current_dir().unwrap_or_default();
        if config.workspace_path.is_absolute() {
            config.workspace_path.clone()
        } else {
            current_dir.join(&config.workspace_path)
        }
    };

    let repo_dir = workspace_dir.join(repo_full_name);
    let worktree_name = format!("pr-{}-{}", pr_number, &commit_sha[..8]);
    let worktree_dir = repo_dir.join("worktrees").join(&worktree_name);

    debug!("workspace_dir: {}", workspace_dir.display());
    debug!("repo_dir: {}", repo_dir.display());
    debug!("worktree_dir: {}", worktree_dir.display());

    let build_result: Result<String> = async {
        fs::create_dir_all(&workspace_dir)
            .await
            .with_context(|| format!("create workspace dir {}", workspace_dir.display()))?;
        prepare_worktree(&repo_dir, &worktree_name, repo_clone_url, pr_number).await?;
        let build_output = run_guix_build(&worktree_dir, config).await?;
        Ok(build_output)
    }
    .await;

    if let Err(e) = cleanup_worktree(&repo_dir, &worktree_name).await {
        warn!("Failed to clean up worktree: {}", e);
    }

    match build_result {
        Ok(build_output) => {
            info!("Guix build successful for PR #{}", pr_number);

            let comment_body = format!(
                "Guix build:\n\n{}\n\n**Hashes:**\n```\n{}\n```",
                config.system_info, build_output
            );
            github::upsert_pr_comment(github_client, repo_full_name, pr_number, &comment_body)
                .await?;
        }
        Err(e) => {
            error!("Guix build failed for PR #{}: {}", pr_number, e);

            let comment_body = format!(
                "Guix build **failed**\n\n{}\n\n<details>\n<summary>Error details (click to expand)</summary>\n\n```text\n{}\n```\n\n</details>",
                config.system_info,
                truncate_output(&format!("{}", e), 2000)
            );

            github::upsert_pr_comment(github_client, repo_full_name, pr_number, &comment_body)
                .await?;
        }
    }

    Ok(())
}

async fn prepare_worktree(
    repo_dir: &Path,
    worktree_name: &str,
    repo_clone_url: &str,
    pr_number: u64,
) -> Result<()> {
    // Ensure main repository exists and is up to date
    if repo_dir.exists() {
        debug!("Main repository exists at {}", repo_dir.display());
        debug!("Fetching latest changes from origin");
        fetch_origin(repo_dir).await?;
        debug!("Fetching PR #{} head ref", pr_number);
        fetch_pr_ref(repo_dir, pr_number).await?;
    } else {
        debug!(
            "Cloning repository {} to {}",
            repo_clone_url,
            repo_dir.display()
        );
        if let Some(parent) = repo_dir.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create parent dir {}", parent.display()))?;
        }
        let output = Command::new("git")
            .current_dir(repo_dir.parent().unwrap_or_else(|| Path::new(".")))
            .args([
                "clone",
                "--filter=blob:none",
                "--no-tags",
                repo_clone_url,
                repo_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("repo"),
            ])
            .output()
            .await
            .with_context(|| format!("git clone {}", repo_clone_url))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("git clone failed: {}", stderr));
        }
        debug!("Fetching PR #{} head ref after clone", pr_number);
        fetch_pr_ref(repo_dir, pr_number).await?;
    }

    info!("Creating worktree for PR #{}", pr_number);

    fs::create_dir_all(repo_dir.join("worktrees"))
        .await
        .with_context(|| format!("create worktrees directory under {}", repo_dir.display()))?;

    debug!("Cleaning up any existing worktree for PR #{}", pr_number);
    let _ = cleanup_worktree(repo_dir, worktree_name).await; // Ignore errors, might not exist

    // Create worktree from PR head ref using relative path
    let pr_ref = format!("refs/remotes/origin/pr/{}/head", pr_number);
    let relative_worktree_path = format!("worktrees/{}", worktree_name);

    let output = tokio::process::Command::new("git")
        .current_dir(repo_dir)
        .args(["worktree", "add", &relative_worktree_path, &pr_ref])
        .output()
        .await
        .with_context(|| "execute git worktree command".to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Git worktree command failed: {}", stderr));
    }
    debug!("Created new worktree for PR #{}", pr_number);

    Ok(())
}

async fn run_guix_build(worktree_dir: &Path, config: &Config) -> Result<String> {
    info!("Running guix build in {}", worktree_dir.display());

    let guix_build_script = worktree_dir.join("contrib/guix/guix-build");
    if !guix_build_script.exists() {
        return Err(anyhow!(
            "Guix build script not found at {}",
            guix_build_script.display()
        ));
    }

    let stdout_file = worktree_dir.join("stdout");
    let stderr_file = worktree_dir.join("stderr");
    info!("Guix build stdout at: {}", stdout_file.display());
    info!("Guix build stderr at: {}", stderr_file.display());

    let stdout_handle = fs::File::create(&stdout_file)
        .await
        .with_context(|| format!("create stdout log file {}", stdout_file.display()))?;
    let stderr_handle = fs::File::create(&stderr_file)
        .await
        .with_context(|| format!("create stderr log file {}", stderr_file.display()))?;

    let absolute_script_path = std::fs::canonicalize(&guix_build_script)
        .with_context(|| format!("canonicalize script path {}", guix_build_script.display()))?;
    let mut cmd = Command::new("bash");
    cmd.arg(&absolute_script_path);
    cmd.current_dir(worktree_dir)
        .stdout(stdout_handle.into_std().await)
        .stderr(stderr_handle.into_std().await);

    // Unset SOURCE_DATE_EPOCH. Nix (doesn't) fix this.
    cmd.env("SOURCE_DATE_EPOCH", "");

    // Add Guix optimization paths if configured
    if let Some(sources_path) = &config.sources_path {
        cmd.env("SOURCES_PATH", sources_path);
        info!("Using SOURCES_PATH: {}", sources_path.display());
    }
    if let Some(base_cache) = &config.base_cache {
        cmd.env("BASE_CACHE", base_cache);
        info!("Using BASE_CACHE: {}", base_cache.display());
    }
    if let Some(sdk_path) = &config.sdk_path {
        cmd.env("SDK_PATH", sdk_path);
        info!("Using SDK_PATH: {}", sdk_path.display());
    }

    cmd.env("JOBS", config.job_count.to_string());
    info!("Using JOBS: {}", config.job_count);

    info!("Starting guix build");
    let output = cmd
        .status()
        .await
        .with_context(|| "execute guix build script".to_string())?;

    if !output.success() {
        let stdout_tail = read_file_tail(&stdout_file, 30)
            .await
            .unwrap_or_else(|_| "Could not read stdout log".to_string());
        let stderr_tail = read_file_tail(&stderr_file, 20)
            .await
            .unwrap_or_else(|_| "Could not read stderr log".to_string());

        let error_output = if stderr_tail.trim().is_empty() {
            stdout_tail
        } else {
            format!("{}\n\nSTDERR:\n{}", stdout_tail, stderr_tail)
        };

        return Err(anyhow!(
            "Guix build failed with exit code {}: {}",
            output.code().unwrap_or(-1),
            error_output
        ));
    }

    info!("Guix build completed successfully");
    info!("Calculating hashes");

    // find guix-build-$(git rev-parse --short=12 HEAD)/output/ -type f -print0 | env LC_ALL=C sort -z | xargs -r0 sha256sum
    // Get short commit hash
    let short_commit_output = Command::new("git")
        .current_dir(worktree_dir)
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .await
        .with_context(|| "get short commit hash".to_string())?;

    if !short_commit_output.status.success() {
        return Err(anyhow!("Failed to get short commit hash"));
    }

    let short_commit = String::from_utf8_lossy(&short_commit_output.stdout)
        .trim()
        .to_string();
    let output_dir = worktree_dir.join(format!("guix-build-{}/output/", short_commit));

    if !output_dir.exists() {
        return Err(anyhow!(
            "Guix output directory not found: {}",
            output_dir.display()
        ));
    }

    let hash_command = format!(
        "find guix-build-{}/output/ -type f -print0 | env LC_ALL=C sort -z | xargs -r0 sha256sum",
        short_commit
    );
    let sha256_cmd = Command::new("sh")
        .current_dir(worktree_dir)
        .args(["-c", &hash_command])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| "execute hash calculation command".to_string())?;

    if !sha256_cmd.status.success() {
        let stderr = String::from_utf8_lossy(&sha256_cmd.stderr);
        return Err(anyhow!("Failed to calculate hashes: {}", stderr));
    }

    let hashes = String::from_utf8_lossy(&sha256_cmd.stdout);
    debug!(
        "Successfully calculated hashes for {} files",
        hashes.lines().count()
    );

    Ok(hashes.to_string())
}

fn truncate_output(output: &str, max_chars: usize) -> String {
    if output.len() <= max_chars {
        output.to_string()
    } else {
        format!(
            "{}...\n[Output truncated - {} characters total]",
            &output[..max_chars],
            output.len()
        )
    }
}

async fn cleanup_worktree(main_repo_dir: &Path, worktree_name: &str) -> Result<()> {
    debug!("Cleaning up worktree: {}", worktree_name);

    let relative_worktree_path = format!("worktrees/{}", worktree_name);
    let output = tokio::process::Command::new("git")
        .current_dir(main_repo_dir)
        .args(["worktree", "remove", "--force", &relative_worktree_path])
        .output()
        .await;

    match output {
        Ok(output) if output.status.success() => {
            debug!("Successfully removed worktree");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Git worktree remove failed: {}", stderr);
        }
        Err(e) => {
            warn!("Failed to execute git worktree remove: {}", e);
        }
    }

    Ok(())
}

async fn read_file_tail(file_path: &Path, max_lines: usize) -> Result<String> {
    let content = fs::read_to_string(file_path)
        .await
        .with_context(|| format!("read file {}", file_path.display()))?;

    Ok(get_last_lines(&content, max_lines))
}

fn get_last_lines(output: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= max_lines {
        output.to_string()
    } else {
        let start_idx = lines.len() - max_lines;
        format!(
            "...\n[Previous {} lines omitted]\n{}",
            start_idx,
            lines[start_idx..].join("\n")
        )
    }
}
