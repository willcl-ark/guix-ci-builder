use rocket::data::ToByteUnit;
use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{
    Request, State, get, post,
    request::{self, FromRequest},
    routes,
};
use serde::Serialize;
use std::sync::Arc;
use tracing::{debug, info, trace, warn};

mod build_manager;
mod config;
mod github;
mod queue;
mod runner;
mod webhook;

use build_manager::BuildManager;
use config::Config;

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    version: String,
    queue_info: Option<QueueInfo>,
}

#[derive(Serialize)]
struct QueueInfo {
    queue_length: usize,
    active_builds: Vec<u64>,
}

#[get("/health")]
async fn health(build_manager: &State<Arc<BuildManager>>) -> Json<HealthResponse> {
    let queue_info = match build_manager.get_queue_status().await {
        Ok(status) => Some(QueueInfo {
            queue_length: status.queue_length,
            active_builds: status.active_builds,
        }),
        Err(_) => None,
    };

    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        queue_info,
    })
}

struct GitHubSignature(Option<String>);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for GitHubSignature {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
        let signature = request
            .headers()
            .get_one("X-Hub-Signature-256")
            .map(|s| s.to_string());
        request::Outcome::Success(GitHubSignature(signature))
    }
}

#[post("/webhook", data = "<payload>")]
async fn webhook_handler(
    payload: String,
    signature: GitHubSignature,
    config: &State<Arc<Config>>,
    build_manager: &State<Arc<BuildManager>>,
) -> Status {
    debug!("Received webhook payload (length: {} bytes)", payload.len());
    debug!("Webhook signature present: {}", signature.0.is_some());
    trace!("Full webhook payload: {}", payload);

    match webhook::handle_webhook(&payload, signature.0.as_deref(), config, build_manager).await {
        Ok(_) => {
            info!("Webhook processed successfully");
            Status::Ok
        }
        Err(e) => {
            warn!("Webhook processing failed: {}", e);
            Status::BadRequest
        }
    }
}

#[rocket::main]
#[allow(clippy::result_large_err)]
async fn main() -> Result<(), rocket::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Starting guix-ci-builder v{}", env!("CARGO_PKG_VERSION"));

    let config = Arc::new(Config::from_env().expect("Failed to load configuration"));
    info!("Configuration loaded successfully");

    let github_client = Arc::new(
        octocrab::OctocrabBuilder::new()
            .personal_token(config.github_token.clone())
            .build()
            .expect("Failed to create GitHub client"),
    );

    let build_manager = Arc::new(
        BuildManager::new(Arc::clone(&config), Arc::clone(&github_client), 100, 1)
            .expect("Failed to create BuildManager"),
    );
    info!("BuildManager initialized successfully");

    let _rocket = rocket::build()
        .configure(rocket::Config {
            cli_colors: false,
            address: config.host,
            port: config.port,
            limits: rocket::data::Limits::default()
                .limit("string", 10_u64.mebibytes())
                .limit("json", 10_u64.mebibytes()),
            ..rocket::Config::default()
        })
        .manage(config)
        .manage(github_client)
        .manage(build_manager)
        .mount("/", routes![health, webhook_handler])
        .launch()
        .await?;

    Ok(())
}
