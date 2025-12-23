use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::auth::ApiKeyAuth;
use crate::db::schema::UploadStatus;
use crate::error::{AppError, Result};
use crate::AppState;

#[derive(Debug, Serialize)]
pub struct CompleteUploadResponse {
    pub file_id: String,
    pub filename: String,
    pub total_size: i64,
    pub status: String,
    pub final_path: String,
    pub storage_backend: String,
}

/// Webhook payload sent when upload completes
#[derive(Debug, Serialize, Deserialize)]
pub struct WebhookPayload {
    pub event: String,
    pub file_id: String,
    pub filename: String,
    pub total_size: i64,
    pub final_path: String,
    pub storage_backend: String,
    pub completed_at: String,
}

pub async fn complete_upload(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CompleteUploadResponse>> {
    // Validate API key
    let api_key = ApiKeyAuth::extract_api_key(&headers)?;
    ApiKeyAuth::validate(&api_key, &state.config.api_key)?;

    // Get upload record
    let upload = state.db.get_upload(&upload_id)?;

    // Check if already complete
    if upload.status == UploadStatus::Complete {
        return Ok(Json(CompleteUploadResponse {
            file_id: upload.id,
            filename: upload.filename,
            total_size: upload.total_size,
            status: "complete".to_string(),
            final_path: upload.final_path.unwrap_or_default(),
            storage_backend: upload.storage_backend,
        }));
    }

    // Check if all parts are uploaded
    if !state.db.all_parts_uploaded(&upload_id)? {
        let uploaded = state.db.count_uploaded_parts(&upload_id)?;
        return Err(AppError::BadRequest(format!(
            "Not all parts uploaded: {}/{} complete",
            uploaded, upload.total_parts
        )));
    }

    tracing::info!(
        "Completing upload: id={}, filename={}, parts={}",
        upload_id,
        upload.filename,
        upload.total_parts
    );

    // Assemble the parts
    let final_path = state
        .storage
        .assemble_parts(&upload_id, &upload.filename, upload.total_parts, upload.target_path.as_deref())
        .await?;

    // Update database
    state.db.update_upload_final_path(&upload_id, &final_path)?;
    state
        .db
        .update_upload_status(&upload_id, UploadStatus::Complete)?;

    tracing::info!("Upload {} completed: {}", upload_id, final_path);

    // Send webhook notification if configured
    if let Some(webhook_url) = &upload.webhook_url {
        let payload = WebhookPayload {
            event: "upload.complete".to_string(),
            file_id: upload.id.clone(),
            filename: upload.filename.clone(),
            total_size: upload.total_size,
            final_path: final_path.clone(),
            storage_backend: upload.storage_backend.clone(),
            completed_at: chrono::Utc::now().to_rfc3339(),
        };

        // Spawn webhook call in background (don't block response)
        let webhook_url = webhook_url.clone();
        tokio::spawn(async move {
            send_webhook(&webhook_url, &payload).await;
        });
    }

    Ok(Json(CompleteUploadResponse {
        file_id: upload.id,
        filename: upload.filename,
        total_size: upload.total_size,
        status: "complete".to_string(),
        final_path,
        storage_backend: upload.storage_backend,
    }))
}

/// Send webhook notification
async fn send_webhook(url: &str, payload: &WebhookPayload) {
    tracing::info!("Sending webhook to: {}", url);

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to create HTTP client for webhook: {}", e);
            return;
        }
    };

    match client
        .post(url)
        .header("Content-Type", "application/json")
        .header("User-Agent", "ChunkedUploader/1.0")
        .json(payload)
        .send()
        .await
    {
        Ok(response) => {
            if response.status().is_success() {
                tracing::info!(
                    "Webhook sent successfully: {} - {}",
                    url,
                    response.status()
                );
            } else {
                tracing::warn!(
                    "Webhook returned non-success status: {} - {}",
                    url,
                    response.status()
                );
            }
        }
        Err(e) => {
            tracing::error!("Failed to send webhook to {}: {}", url, e);
        }
    }
}
