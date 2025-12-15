use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, Result};

#[derive(Debug, Serialize, Deserialize)]
pub struct PartClaims {
    pub upload_id: String,
    pub part_number: i32,
    pub expected_size: i64,
    pub exp: u64,
}

pub struct PartTokenGenerator {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
}

impl PartTokenGenerator {
    pub fn new(secret: &str) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
        }
    }

    /// Generate a JWT token for a specific upload part
    pub fn generate_token(
        &self,
        upload_id: &str,
        part_number: i32,
        expected_size: i64,
        expires_at: u64,
    ) -> Result<String> {
        let claims = PartClaims {
            upload_id: upload_id.to_string(),
            part_number,
            expected_size,
            exp: expires_at,
        };

        encode(&Header::default(), &claims, &self.encoding_key)
            .map_err(|e| AppError::Internal(format!("Failed to generate token: {}", e)))
    }

    /// Validate and decode a JWT token
    pub fn validate_token(&self, token: &str) -> Result<PartClaims> {
        let validation = Validation::default();
        let token_data = decode::<PartClaims>(token, &self.decoding_key, &validation)?;
        Ok(token_data.claims)
    }

    /// Generate SHA256 hash of a token for storage
    pub fn hash_token(token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        hex::encode(hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_generation_and_validation() {
        let generator = PartTokenGenerator::new("test-secret");
        let expires_at = chrono::Utc::now().timestamp() as u64 + 3600;

        let token = generator
            .generate_token("upload-123", 0, 1024, expires_at)
            .unwrap();

        let claims = generator.validate_token(&token).unwrap();
        assert_eq!(claims.upload_id, "upload-123");
        assert_eq!(claims.part_number, 0);
        assert_eq!(claims.expected_size, 1024);
    }

    #[test]
    fn test_token_hash() {
        let token = "test-token";
        let hash = PartTokenGenerator::hash_token(token);
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 produces 64 hex chars
    }
}

