use axum::{extract::State, http::HeaderMap, Json};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::{ApiKeyAuth, PartTokenGenerator};
use crate::db::schema::{PartStatus, Upload, UploadPart, UploadStatus};
use crate::error::Result;
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct InitUploadRequest {
    pub filename: String, // Can include path: "videos/2024/movie.mp4"
    pub total_size: i64,
    #[serde(default)]
    pub checksum_sha256: Option<String>,
    #[serde(default)]
    pub webhook_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InitUploadResponse {
    pub file_id: String,
    pub total_parts: i32,
    pub chunk_size: i64,
    pub parts: Vec<PartInfo>,
    pub expires_at: String,
}

#[derive(Debug, Serialize)]
pub struct PartInfo {
    pub part: i32,
    pub token: String,
    pub status: String,
}

pub async fn init_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<InitUploadRequest>,
) -> Result<Json<InitUploadResponse>> {
    // Validate API key
    let api_key = ApiKeyAuth::extract_api_key(&headers)?;
    ApiKeyAuth::validate(&api_key, &state.config.api_key)?;

    // Calculate number of parts
    let chunk_size = state.config.chunk_size_bytes as i64;
    let total_parts = ((request.total_size as f64) / (chunk_size as f64)).ceil() as i32;

    // Generate upload ID
    let upload_id = Uuid::new_v4().to_string();

    // Calculate expiration
    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::hours(state.config.upload_ttl_hours as i64);
    let expires_timestamp = expires_at.timestamp();

    // Extract path and filename from the provided filename
    // e.g., "videos/2024/movie.mp4" -> path: "videos/2024", filename: "movie.mp4"
    let (target_path, actual_filename) = {
        let path = std::path::Path::new(&request.filename);
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&request.filename)
            .to_string();

        let parent = path
            .parent()
            .and_then(|p| p.to_str())
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string());

        (parent, filename)
    };

    tracing::info!(
        "Initializing upload: id={}, filename={}, path={:?}, size={}, parts={}",
        upload_id,
        actual_filename,
        target_path,
        request.total_size,
        total_parts
    );

    // Create upload record
    let upload = Upload {
        id: upload_id.clone(),
        filename: actual_filename,
        total_size: request.total_size,
        chunk_size,
        total_parts,
        status: UploadStatus::Pending,
        storage_backend: state.storage.backend_type().to_string(),
        target_path,
        final_path: None,
        checksum_sha256: request.checksum_sha256,
        webhook_url: request.webhook_url,
        finalization_started_at: None,
        finalization_updated_at: None,
        finalization_error: None,
        finalizing_progress_percent: 0,
        created_at: now.timestamp(),
        updated_at: now.timestamp(),
        expires_at: expires_timestamp,
    };

    state.db.create_upload(&upload)?;

    // Generate JWT tokens for each part
    let token_generator = PartTokenGenerator::new(&state.config.jwt_secret);
    let mut parts = Vec::with_capacity(total_parts as usize);
    let mut db_parts = Vec::with_capacity(total_parts as usize);

    for part_num in 0..total_parts {
        // Calculate expected size for this part
        let expected_size = if part_num == total_parts - 1 {
            // Last part may be smaller
            let remaining = request.total_size % chunk_size;
            if remaining == 0 {
                chunk_size
            } else {
                remaining
            }
        } else {
            chunk_size
        };

        // Generate token
        let token = token_generator.generate_token(
            &upload_id,
            part_num,
            expected_size,
            expires_timestamp as u64,
        )?;

        let token_hash = PartTokenGenerator::hash_token(&token);

        parts.push(PartInfo {
            part: part_num,
            token: token.clone(),
            status: "pending".to_string(),
        });

        db_parts.push(UploadPart {
            upload_id: upload_id.clone(),
            part_number: part_num,
            token_hash,
            status: PartStatus::Pending,
            size: expected_size,
            checksum_sha256: None,
            uploaded_at: None,
        });
    }

    // Store parts in database
    state.db.create_parts(&db_parts)?;

    Ok(Json(InitUploadResponse {
        file_id: upload_id,
        total_parts,
        chunk_size,
        parts,
        expires_at: expires_at.to_rfc3339(),
    }))
}
