use axum::{
    extract::Request,
    http::HeaderMap,
    middleware::Next,
    response::Response,
};

use crate::error::{AppError, Result};

const API_KEY_HEADER: &str = "X-API-Key";

pub struct ApiKeyAuth;

impl ApiKeyAuth {
    pub fn extract_api_key(headers: &HeaderMap) -> Result<String> {
        headers
            .get(API_KEY_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| AppError::Unauthorized("Missing API key".to_string()))
    }

    pub fn validate(provided: &str, expected: &str) -> Result<()> {
        if provided == expected {
            Ok(())
        } else {
            Err(AppError::Unauthorized("Invalid API key".to_string()))
        }
    }
}

/// Middleware for API key validation (optional, can be used as layer)
#[allow(dead_code)]
pub async fn api_key_middleware(
    request: Request,
    next: Next,
) -> std::result::Result<Response, AppError> {
    // This middleware is optional - handlers validate API key directly
    // to access AppState for the expected key
    Ok(next.run(request).await)
}

