use anyhow::{Context, Result};
use std::env;

#[derive(Debug, Clone, PartialEq)]
pub enum StorageBackendType {
    Local,
    Smb,
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
    pub temp_storage_path: String, // Local temp path for parts (fast SSD recommended)

    // SMB/NAS Configuration
    pub smb_host: String,
    pub smb_port: u16,
    pub smb_user: String,
    pub smb_pass: String,
    pub smb_share: String,          // Share name on the server
    pub smb_path: String,           // Subdirectory within the share
    pub smb_mount_point: String,    // Local mount point

    // S3 Configuration
    pub s3_endpoint: Option<String>,
    pub s3_bucket: String,
    pub s3_region: String,

    // Upload settings
    pub chunk_size_mb: u64,
    pub chunk_size_bytes: u64,
    pub upload_ttl_hours: u64,
    pub max_concurrent_finalizations: usize,

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
            "smb" => StorageBackendType::Smb,
            _ => StorageBackendType::Local,
        };

        let local_storage_path =
            env::var("LOCAL_STORAGE_PATH").unwrap_or_else(|_| "./uploads".to_string());

        // Temp storage for parts - defaults to system temp dir for fast local I/O
        let temp_storage_path = env::var("TEMP_STORAGE_PATH")
            .unwrap_or_else(|_| env::temp_dir().join("chunked-uploads").to_string_lossy().to_string());

        // SMB/NAS Configuration
        let smb_host = env::var("SMB_HOST").unwrap_or_else(|_| "localhost".to_string());
        let smb_port: u16 = env::var("SMB_PORT")
            .unwrap_or_else(|_| "445".to_string())
            .parse()
            .context("Invalid SMB_PORT")?;
        let smb_user = env::var("SMB_USER").unwrap_or_default();
        let smb_pass = env::var("SMB_PASS").unwrap_or_default();
        let smb_share = env::var("SMB_SHARE").unwrap_or_else(|_| "share".to_string());
        let smb_path = env::var("SMB_PATH").unwrap_or_default(); // Subdirectory within share
        let smb_mount_point = env::var("SMB_MOUNT_POINT")
            .unwrap_or_else(|_| "/Volumes/uploads".to_string());

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

        let max_concurrent_finalizations: usize = env::var("MAX_CONCURRENT_FINALIZATIONS")
            .unwrap_or_else(|_| "4".to_string())
            .parse()
            .context("Invalid MAX_CONCURRENT_FINALIZATIONS")?;

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
            temp_storage_path,
            smb_host,
            smb_port,
            smb_user,
            smb_pass,
            smb_share,
            smb_path,
            smb_mount_point,
            s3_endpoint,
            s3_bucket,
            s3_region,
            chunk_size_mb,
            chunk_size_bytes: chunk_size_mb * 1024 * 1024,
            upload_ttl_hours,
            max_concurrent_finalizations,
            database_path,
            server_port,
        })
    }
}
