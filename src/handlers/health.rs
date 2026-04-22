use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;

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

pub async fn health_check(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let backend_type = state.storage.backend_type().to_string();

    // Use the backend's health_check method
    let (is_healthy, message) = state.storage.health_check().await;

    let storage_status = if is_healthy { "healthy" } else { "unavailable" };
    let overall_status = if is_healthy { "OK" } else { "DEGRADED" };
    let http_status = if is_healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        http_status,
        Json(HealthResponse {
            status: overall_status.to_string(),
            storage: StorageInfo {
                backend_type,
                status: storage_status.to_string(),
                message,
            },
        }),
    )
}
