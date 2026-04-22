use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde::Serialize;

use crate::auth::ApiKeyAuth;
use crate::db::schema::UploadStatus;
use crate::error::Result;
use crate::AppState;

#[derive(Debug, Serialize)]
pub struct CancelUploadResponse {
    pub file_id: String,
    pub status: String,
    pub message: String,
}

pub async fn cancel_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CancelUploadResponse>> {
    // Validate API key
    let api_key = ApiKeyAuth::extract_api_key(&headers)?;
    ApiKeyAuth::validate(&api_key, &state.config.api_key)?;

    // Get upload record
    let upload = state.db.get_upload(&upload_id)?;

    tracing::info!(
        "Cancelling upload: id={}, status={}",
        upload_id,
        upload.status
    );

    if upload.status == UploadStatus::Finalizing {
        return Err(crate::error::AppError::Conflict(
            "Upload is finalizing and cannot be cancelled".to_string(),
        ));
    }

    // Clean up storage
    if upload.status == UploadStatus::Complete {
        // Delete the final file if it exists
        if let Some(final_path) = &upload.final_path {
            if let Err(e) = state.storage.delete_file(final_path).await {
                tracing::warn!("Failed to delete final file: {}", e);
            }
        }
    } else {
        // Delete any incomplete upload artifacts.
        if let Err(e) = state.storage.cleanup_incomplete_upload(&upload).await {
            tracing::warn!("Failed to cleanup incomplete upload: {}", e);
        }
    }

    // Delete from database
    state.db.delete_upload(&upload_id)?;

    Ok(Json(CancelUploadResponse {
        file_id: upload_id,
        status: "cancelled".to_string(),
        message: "Upload cancelled and cleaned up".to_string(),
    }))
}
