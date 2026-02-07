use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::auth::ApiKeyAuth;
use crate::db::schema::UploadStatus;
use crate::error::Result;
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    #[serde(default)]
    pub include_parts: bool,
}

#[derive(Debug, Serialize)]
pub struct UploadStatusResponse {
    pub file_id: String,
    pub filename: String,
    pub total_size: i64,
    pub chunk_size: i64,
    pub total_parts: i32,
    pub uploaded_parts: i32,
    pub status: String,
    pub phase: String,
    pub upload_progress_percent: f64,
    pub finalizing_progress_percent: i32,
    pub finalization_error: Option<String>,
    pub storage_backend: String,
    pub parts: Option<Vec<PartStatusInfo>>,
    pub created_at: String,
    pub expires_at: String,
    pub final_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PartStatusInfo {
    pub part: i32,
    pub status: String,
    pub checksum_sha256: Option<String>,
    pub uploaded_at: Option<String>,
}

pub async fn get_status(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
    Query(query): Query<StatusQuery>,
    headers: HeaderMap,
) -> Result<Json<UploadStatusResponse>> {
    let api_key = ApiKeyAuth::extract_api_key(&headers)?;
    ApiKeyAuth::validate(&api_key, &state.config.api_key)?;

    let upload = state.db.get_upload(&upload_id)?;

    let uploaded_count = state.db.count_uploaded_parts(&upload_id)?;

    let upload_progress_percent = if upload.total_parts > 0 {
        (uploaded_count as f64 / upload.total_parts as f64) * 100.0
    } else {
        0.0
    };

    let phase = match upload.status {
        UploadStatus::Pending => "uploading",
        UploadStatus::Finalizing => "finalizing",
        UploadStatus::Complete => "complete",
        UploadStatus::Failed => "failed",
    }
    .to_string();

    let parts = if query.include_parts {
        let rows = state.db.get_all_parts(&upload_id)?;
        Some(
            rows.iter()
                .map(|p| PartStatusInfo {
                    part: p.part_number,
                    status: p.status.to_string(),
                    checksum_sha256: p.checksum_sha256.clone(),
                    uploaded_at: p.uploaded_at.map(|ts| {
                        chrono::DateTime::from_timestamp(ts, 0)
                            .map(|dt| dt.to_rfc3339())
                            .unwrap_or_default()
                    }),
                })
                .collect(),
        )
    } else {
        None
    };

    let created_at = chrono::DateTime::from_timestamp(upload.created_at, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    let expires_at = chrono::DateTime::from_timestamp(upload.expires_at, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    Ok(Json(UploadStatusResponse {
        file_id: upload.id,
        filename: upload.filename,
        total_size: upload.total_size,
        chunk_size: upload.chunk_size,
        total_parts: upload.total_parts,
        uploaded_parts: uploaded_count,
        status: upload.status.to_string(),
        phase,
        upload_progress_percent,
        finalizing_progress_percent: upload.finalizing_progress_percent,
        finalization_error: upload.finalization_error,
        storage_backend: upload.storage_backend,
        parts,
        created_at,
        expires_at,
        final_path: upload.final_path,
    }))
}
