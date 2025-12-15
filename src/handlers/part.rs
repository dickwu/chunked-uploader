use axum::{
    body::Bytes,
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::auth::PartTokenGenerator;
use crate::db::schema::{PartStatus, UploadStatus};
use crate::error::{AppError, Result};
use crate::AppState;

const AUTHORIZATION_HEADER: &str = "Authorization";

#[derive(Debug, Serialize)]
pub struct PartUploadResponse {
    pub upload_id: String,
    pub part_number: i32,
    pub status: String,
    pub checksum_sha256: String,
    pub uploaded_parts: i32,
    pub total_parts: i32,
}

pub async fn upload_part(
    State(state): State<AppState>,
    Path((upload_id, part_num)): Path<(String, i32)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<PartUploadResponse>> {
    // Extract JWT token from Authorization header
    let token = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| AppError::Unauthorized("Missing or invalid Authorization header".to_string()))?;

    // Validate JWT token
    let token_generator = PartTokenGenerator::new(&state.config.jwt_secret);
    let claims = token_generator.validate_token(token)?;

    // Verify token matches the request
    if claims.upload_id != upload_id || claims.part_number != part_num {
        return Err(AppError::Unauthorized(
            "Token does not match upload/part".to_string(),
        ));
    }

    // Get upload record
    let upload = state.db.get_upload(&upload_id)?;

    // Check upload status
    if upload.status != UploadStatus::Pending {
        return Err(AppError::Conflict(format!(
            "Upload is already {}",
            upload.status
        )));
    }

    // Get part record
    let part = state.db.get_part(&upload_id, part_num)?;

    // Check if part already uploaded
    if part.status == PartStatus::Uploaded {
        return Err(AppError::Conflict("Part already uploaded".to_string()));
    }

    // Validate token hash
    let token_hash = PartTokenGenerator::hash_token(token);
    if token_hash != part.token_hash {
        return Err(AppError::Unauthorized("Invalid token for this part".to_string()));
    }

    // Validate size
    let body_len = body.len() as i64;
    if body_len != claims.expected_size {
        return Err(AppError::BadRequest(format!(
            "Part size mismatch: expected {}, got {}",
            claims.expected_size, body_len
        )));
    }

    // Calculate checksum
    let mut hasher = Sha256::new();
    hasher.update(&body);
    let checksum = hex::encode(hasher.finalize());

    tracing::info!(
        "Uploading part {} for upload {} ({} bytes, checksum: {})",
        part_num,
        upload_id,
        body_len,
        &checksum[..16]
    );

    // Store the part
    state
        .storage
        .store_part(&upload_id, part_num, body)
        .await?;

    // Update part status in database
    state
        .db
        .update_part_status(&upload_id, part_num, PartStatus::Uploaded, Some(&checksum))?;

    // Get updated counts
    let uploaded_count = state.db.count_uploaded_parts(&upload_id)?;

    Ok(Json(PartUploadResponse {
        upload_id,
        part_number: part_num,
        status: "uploaded".to_string(),
        checksum_sha256: checksum,
        uploaded_parts: uploaded_count,
        total_parts: upload.total_parts,
    }))
}

