use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;
use std::path::PathBuf;

use crate::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub storage: StorageInfo,
}

#[derive(Serialize)]
pub struct StorageInfo {
    pub backend_type: String,
    pub status: String,
    pub message: Option<String>,
}

pub async fn health_check(State(state): State<AppState>) -> Result<Json<HealthResponse>, StatusCode> {
    let backend_type = state.storage.backend_type().to_string();
    
    // Simple, fast health check - avoid blocking operations
    // For production, you might want more thorough checks, but keep them fast
    let (storage_status, storage_message) = match backend_type.as_str() {
        "local" => {
            // For local storage, just check if temp directory exists or can be created
            let temp_path = PathBuf::from(&state.config.temp_storage_path);
            if temp_path.exists() {
                ("healthy", None)
            } else {
                // Try to create the directory (non-blocking check)
                match std::fs::create_dir_all(&temp_path) {
                    Ok(_) => ("healthy", None),
                    Err(e) => ("degraded", Some(format!("Temp storage issue: {}", e))),
                }
            }
        }
        "smb" => {
            // For SMB, assume healthy if backend is initialized
            // Actual connection was tested during startup
            // Avoid testing connection here to keep health check fast
            ("healthy", Some("SMB backend initialized".to_string()))
        }
        "s3" => {
            // For S3, similar approach
            ("healthy", Some("S3 backend initialized".to_string()))
        }
        _ => ("unknown", Some("Unknown storage backend".to_string())),
    };

    // Always return 200 OK for health check
    // Status details are in the response body
    Ok(Json(HealthResponse {
        status: "OK".to_string(),
        storage: StorageInfo {
            backend_type,
            status: storage_status.to_string(),
            message: storage_message,
        },
    }))
}
