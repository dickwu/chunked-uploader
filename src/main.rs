mod auth;
mod config;
mod db;
mod error;
mod handlers;
mod services;
mod storage;

use axum::{
    extract::DefaultBodyLimit,
    routing::{delete, get, post, put},
    Router,
};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::Config;
use crate::db::Database;
use crate::services::cleanup::CleanupService;
use crate::storage::StorageBackend;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Arc<Database>,
    pub storage: Arc<dyn StorageBackend>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "chunked_uploader=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load configuration
    dotenvy::dotenv().ok();
    let config = Config::from_env()?;
    let config = Arc::new(config);

    tracing::info!("Starting chunked upload server...");
    tracing::info!("Storage backend: {:?}", config.storage_backend);
    tracing::info!("Chunk size: {} MB", config.chunk_size_mb);

    // Initialize database
    let db = Database::new(&config.database_path)?;
    db.run_migrations()?;
    let db = Arc::new(db);

    // Initialize storage backend
    let storage: Arc<dyn StorageBackend> = match config.storage_backend {
        config::StorageBackendType::Local => {
            tracing::info!("Temp storage: {}", config.temp_storage_path);
            tracing::info!("Final storage: {}", config.local_storage_path);
            Arc::new(storage::local::LocalStorage::new(
                &config.local_storage_path,
                &config.temp_storage_path,
            )?)
        }
        #[cfg(feature = "smb")]
        config::StorageBackendType::Smb => {
            tracing::info!("Temp storage: {}", config.temp_storage_path);
            tracing::info!("SMB: smb://{}@{}:{}/{}", config.smb_user, config.smb_host, config.smb_port, config.smb_share);
            Arc::new(storage::smb::SmbStorage::new(
                &config,
                &config.temp_storage_path,
            ).await?)
        }
        #[cfg(not(feature = "smb"))]
        config::StorageBackendType::Smb => {
            anyhow::bail!("SMB storage backend requires the 'smb' feature. Rebuild with: cargo build --features smb")
        }
        #[cfg(feature = "s3")]
        config::StorageBackendType::S3 => {
            Arc::new(storage::s3::S3Storage::new(&config).await?)
        }
        #[cfg(not(feature = "s3"))]
        config::StorageBackendType::S3 => {
            anyhow::bail!("S3 storage backend requires the 's3' feature. Rebuild with: cargo build --features s3")
        }
    };

    // Create app state
    let state = AppState {
        config: config.clone(),
        db: db.clone(),
        storage,
    };

    // Start cleanup service
    let cleanup_service = CleanupService::new(db.clone(), state.storage.clone(), config.clone());
    tokio::spawn(async move {
        cleanup_service.run().await;
    });

    // Build router
    // Set body limit to chunk_size + 1MB buffer (default chunk is 50MB)
    let body_limit = (config.chunk_size_bytes + 1024 * 1024) as usize;
    tracing::info!("Max body size: {} MB", body_limit / 1024 / 1024);

    let app = Router::new()
        .route("/upload/init", post(handlers::init::init_upload))
        .route(
            "/upload/{id}/part/{part_num}",
            put(handlers::part::upload_part),
        )
        .route("/upload/{id}/status", get(handlers::status::get_status))
        .route(
            "/upload/{id}/complete",
            post(handlers::complete::complete_upload),
        )
        .route("/upload/{id}", delete(handlers::cancel::cancel_upload))
        .route("/health", get(handlers::health::health_check))
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Start server
    let addr = format!("0.0.0.0:{}", config.server_port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Server listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
