use anyhow::{Context, Result};
use std::env;

#[derive(Debug, Clone, PartialEq)]
pub enum StorageBackendType {
    Local,
    S3,
}

#[derive(Debug, Clone)]
pub struct Config {
    // Authentication
    pub api_key: String,
    pub jwt_secret: String,

    // Storage
    pub storage_backend: StorageBackendType,
    pub local_storage_path: String,

    // S3 Configuration
    pub s3_endpoint: Option<String>,
    pub s3_bucket: String,
    pub s3_region: String,

    // Upload settings
    pub chunk_size_mb: u64,
    pub chunk_size_bytes: u64,
    pub upload_ttl_hours: u64,

    // Server
    pub database_path: String,
    pub server_port: u16,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let api_key = env::var("API_KEY").context("API_KEY must be set")?;
        let jwt_secret = env::var("JWT_SECRET").context("JWT_SECRET must be set")?;

        let storage_backend = match env::var("STORAGE_BACKEND")
            .unwrap_or_else(|_| "local".to_string())
            .to_lowercase()
            .as_str()
        {
            "s3" => StorageBackendType::S3,
            _ => StorageBackendType::Local,
        };

        let local_storage_path =
            env::var("LOCAL_STORAGE_PATH").unwrap_or_else(|_| "./uploads".to_string());

        let s3_endpoint = env::var("S3_ENDPOINT").ok();
        let s3_bucket = env::var("S3_BUCKET").unwrap_or_else(|_| "uploads".to_string());
        let s3_region = env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());

        let chunk_size_mb: u64 = env::var("CHUNK_SIZE_MB")
            .unwrap_or_else(|_| "50".to_string())
            .parse()
            .context("Invalid CHUNK_SIZE_MB")?;

        let upload_ttl_hours: u64 = env::var("UPLOAD_TTL_HOURS")
            .unwrap_or_else(|_| "24".to_string())
            .parse()
            .context("Invalid UPLOAD_TTL_HOURS")?;

        let database_path =
            env::var("DATABASE_PATH").unwrap_or_else(|_| "./uploads.db".to_string());

        let server_port: u16 = env::var("SERVER_PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse()
            .context("Invalid SERVER_PORT")?;

        Ok(Config {
            api_key,
            jwt_secret,
            storage_backend,
            local_storage_path,
            s3_endpoint,
            s3_bucket,
            s3_region,
            chunk_size_mb,
            chunk_size_bytes: chunk_size_mb * 1024 * 1024,
            upload_ttl_hours,
            database_path,
            server_port,
        })
    }
}

