use anyhow::{Result, anyhow};
use std::env;
use std::net::IpAddr;
use std::path::PathBuf;
use sysinfo::System;

#[derive(Debug, Clone)]
pub struct Config {
    pub github_token: String,
    pub webhook_secret: String,
    pub host: IpAddr,
    pub port: u16,
    pub workspace_path: PathBuf,
    pub sources_path: Option<PathBuf>,
    pub base_cache: Option<PathBuf>,
    pub sdk_path: Option<PathBuf>,
    pub job_count: usize,
    pub system_info: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let github_token = env::var("GITHUB_TOKEN")
            .map_err(|_| anyhow!("GITHUB_TOKEN environment variable is required"))?;

        let webhook_secret = env::var("WEBHOOK_SECRET")
            .map_err(|_| anyhow!("WEBHOOK_SECRET environment variable is required"))?;

        let host = env::var("GB_HOST")
            .unwrap_or_else(|_| "0.0.0.0".to_string())
            .parse::<IpAddr>()
            .map_err(|_| anyhow!("Invalid GB_HOST value"))?;

        let port = env::var("GB_PORT")
            .unwrap_or_else(|_| "8000".to_string())
            .parse::<u16>()
            .map_err(|_| anyhow!("Invalid GB_PORT value"))?;

        let workspace_path = env::var("WORKSPACE_PATH")
            .unwrap_or_else(|_| "/tmp/workspace".to_string())
            .into();

        let sources_path = env::var("SOURCES_PATH").ok().map(PathBuf::from);
        let base_cache = env::var("BASE_CACHE").ok().map(PathBuf::from);
        let sdk_path = env::var("SDK_PATH").ok().map(PathBuf::from);

        // Calculate job count: (cores - 2) / 2, minimum 1
        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        // let job_count = ((cpu_count.saturating_sub(2)) / 2).max(1);
        let job_count = cpu_count;

        let mut sys = System::new_all();
        sys.refresh_all();
        let total_memory_gb = sys.total_memory() / (1024 * 1024 * 1024);
        let cpu_brand = sys.cpus()[0].brand();
        let system_name = System::name().unwrap_or_else(|| "Unknown".to_string());
        let os_version = System::os_version().unwrap_or_else(|| "Unknown".to_string());
        let architecture = System::cpu_arch().to_string();
        let system_info = format!(
            "- CPU: {} ({} cores, {} build jobs)\n- RAM: {} GB\n- OS: {} {}\n- Arch: {}",
            cpu_brand, cpu_count, job_count, total_memory_gb, system_name, os_version, architecture
        );

        Ok(Config {
            github_token,
            webhook_secret,
            host,
            port,
            workspace_path,
            sources_path,
            base_cache,
            sdk_path,
            job_count,
            system_info,
        })
    }
}
