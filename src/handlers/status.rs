use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde::Serialize;

use crate::auth::ApiKeyAuth;
use crate::db::schema::PartStatus;
use crate::error::Result;
use crate::AppState;

#[derive(Debug, Serialize)]
pub struct UploadStatusResponse {
    pub file_id: String,
    pub filename: String,
    pub total_size: i64,
    pub chunk_size: i64,
    pub total_parts: i32,
    pub uploaded_parts: i32,
    pub status: String,
    pub progress_percent: f64,
    pub parts: Vec<PartStatusInfo>,
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
    headers: HeaderMap,
) -> Result<Json<UploadStatusResponse>> {
    // Validate API key
    let api_key = ApiKeyAuth::extract_api_key(&headers)?;
    ApiKeyAuth::validate(&api_key, &state.config.api_key)?;

    // Get upload record
    let upload = state.db.get_upload(&upload_id)?;

    // Get all parts
    let parts = state.db.get_all_parts(&upload_id)?;

    // Calculate progress
    let uploaded_count = parts
        .iter()
        .filter(|p| p.status == PartStatus::Uploaded)
        .count() as i32;

    let progress_percent = if upload.total_parts > 0 {
        (uploaded_count as f64 / upload.total_parts as f64) * 100.0
    } else {
        0.0
    };

    // Build part status list
    let parts_status: Vec<PartStatusInfo> = parts
        .iter()
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
        .collect();

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
        progress_percent,
        parts: parts_status,
        created_at,
        expires_at,
        final_path: upload.final_path,
    }))
}

