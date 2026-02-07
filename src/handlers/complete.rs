use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::auth::ApiKeyAuth;
use crate::db::schema::{PartStatus, UploadStatus};
use crate::error::{AppError, Result};
use crate::AppState;

#[derive(Debug, Serialize)]
pub struct CompleteUploadResponse {
    pub file_id: String,
    pub filename: String,
    pub total_size: i64,
    pub status: String,
    pub phase: String,
    pub final_path: Option<String>,
    pub storage_backend: String,
    pub finalizing_progress_percent: i32,
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
) -> Result<(StatusCode, Json<CompleteUploadResponse>)> {
    let api_key = ApiKeyAuth::extract_api_key(&headers)?;
    ApiKeyAuth::validate(&api_key, &state.config.api_key)?;

    let upload = state.db.get_upload(&upload_id)?;

    if upload.status == UploadStatus::Complete {
        return Ok((
            StatusCode::OK,
            Json(CompleteUploadResponse {
                file_id: upload.id,
                filename: upload.filename,
                total_size: upload.total_size,
                status: "complete".to_string(),
                phase: "complete".to_string(),
                final_path: upload.final_path,
                storage_backend: upload.storage_backend,
                finalizing_progress_percent: 100,
            }),
        ));
    }

    if upload.status == UploadStatus::Finalizing {
        // Re-trigger if finalization has been stale for >5 minutes (dead task from restart/crash)
        const STALE_THRESHOLD_SECS: i64 = 300;
        if let Ok(true) = state.db.restart_stale_finalization(&upload_id, STALE_THRESHOLD_SECS) {
            tracing::warn!(
                "Re-triggering stale finalization for upload {} (no progress for >{}s)",
                upload_id,
                STALE_THRESHOLD_SECS
            );

            let state_clone = state.clone();
            let upload_id_clone = upload_id.clone();
            tokio::spawn(async move {
                run_finalization_task(state_clone, upload_id_clone).await;
            });

            let current = state.db.get_upload(&upload_id)?;
            return Ok((
                StatusCode::ACCEPTED,
                Json(CompleteUploadResponse {
                    file_id: current.id,
                    filename: current.filename,
                    total_size: current.total_size,
                    status: "finalizing".to_string(),
                    phase: "finalizing".to_string(),
                    final_path: current.final_path,
                    storage_backend: current.storage_backend,
                    finalizing_progress_percent: current.finalizing_progress_percent,
                }),
            ));
        }

        return Ok((
            StatusCode::ACCEPTED,
            Json(CompleteUploadResponse {
                file_id: upload.id,
                filename: upload.filename,
                total_size: upload.total_size,
                status: "finalizing".to_string(),
                phase: "finalizing".to_string(),
                final_path: upload.final_path,
                storage_backend: upload.storage_backend,
                finalizing_progress_percent: upload.finalizing_progress_percent,
            }),
        ));
    }

    if !state.db.all_parts_uploaded(&upload_id)? {
        let uploaded = state.db.count_uploaded_parts(&upload_id)?;
        return Err(AppError::BadRequest(format!(
            "Not all parts uploaded: {}/{} complete",
            uploaded, upload.total_parts
        )));
    }

    let started = state.db.try_start_finalization(&upload_id)?;
    if started {
        tracing::info!(
            "Queued upload finalization: id={}, filename={}, parts={}",
            upload_id,
            upload.filename,
            upload.total_parts
        );

        let state_clone = state.clone();
        let upload_id_clone = upload_id.clone();
        tokio::spawn(async move {
            run_finalization_task(state_clone, upload_id_clone).await;
        });
    }

    let current = state.db.get_upload(&upload_id)?;
    let status_code = if current.status == UploadStatus::Complete {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };

    let response = CompleteUploadResponse {
        file_id: current.id,
        filename: current.filename,
        total_size: current.total_size,
        status: current.status.to_string(),
        phase: match current.status {
            UploadStatus::Complete => "complete".to_string(),
            UploadStatus::Finalizing => "finalizing".to_string(),
            UploadStatus::Failed => "failed".to_string(),
            UploadStatus::Pending => "uploading".to_string(),
        },
        final_path: current.final_path,
        storage_backend: current.storage_backend,
        finalizing_progress_percent: current.finalizing_progress_percent,
    };

    Ok((status_code, Json(response)))
}

pub async fn run_finalization_task(state: AppState, upload_id: String) {
    let _permit = match state.finalization_semaphore.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(e) => {
            tracing::error!("Failed to acquire finalization semaphore for {}: {}", upload_id, e);
            return;
        }
    };

    let upload = match state.db.get_upload(&upload_id) {
        Ok(upload) => upload,
        Err(e) => {
            tracing::error!("Failed to load upload {} for finalization: {}", upload_id, e);
            return;
        }
    };

    if let Err(e) = state.db.update_finalizing_progress(&upload_id, 1) {
        tracing::warn!("Failed to set finalizing progress for {}: {}", upload_id, e);
    }

    let parts = match state.db.get_all_parts(&upload_id) {
        Ok(parts) => parts,
        Err(e) => {
            mark_finalization_failed(&state, &upload_id, &format!("Failed to load parts: {}", e));
            return;
        }
    };

    let total = parts.len().max(1);
    for (index, part) in parts.iter().enumerate() {
        if part.status != PartStatus::Uploaded {
            mark_finalization_failed(
                &state,
                &upload_id,
                &format!(
                    "Finalization verification failed: part {} is not uploaded",
                    part.part_number
                ),
            );
            return;
        }

        let progress = 1 + (((index + 1) as i32 * 89) / (total as i32));
        if let Err(e) = state.db.update_finalizing_progress(&upload_id, progress) {
            tracing::warn!("Failed to update finalizing progress for {}: {}", upload_id, e);
        }
    }

    if let Err(e) = state.storage.verify_upload_ready(&upload).await {
        mark_finalization_failed(
            &state,
            &upload_id,
            &format!("Finalization verification failed: {}", e),
        );
        return;
    }

    if let Err(e) = state.db.update_finalizing_progress(&upload_id, 95) {
        tracing::warn!("Failed to update finalizing progress for {}: {}", upload_id, e);
    }

    let final_path = match state.storage.finalize_upload(&upload).await {
        Ok(path) => path,
        Err(e) => {
            mark_finalization_failed(
                &state,
                &upload_id,
                &format!("Finalization failed: {}", e),
            );
            return;
        }
    };

    if let Err(e) = state.db.mark_finalization_complete(&upload_id, &final_path) {
        tracing::error!(
            "Failed to mark upload {} complete after finalization: {}",
            upload_id,
            e
        );
        return;
    }

    tracing::info!("Upload {} finalized successfully at {}", upload_id, final_path);

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

        let webhook_url = webhook_url.clone();
        tokio::spawn(async move {
            send_webhook(&webhook_url, &payload).await;
        });
    }
}

fn mark_finalization_failed(state: &AppState, upload_id: &str, message: &str) {
    tracing::error!("{}", message);
    if let Err(e) = state.db.mark_finalization_failed(upload_id, message) {
        tracing::error!(
            "Failed to persist finalization failure for {}: {}",
            upload_id,
            e
        );
    }
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
        .header("User-Agent", "ChunkedUploader/2.0")
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
