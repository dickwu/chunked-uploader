use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Upload {
    pub id: String,
    pub filename: String,
    pub total_size: i64,
    pub chunk_size: i64,
    pub total_parts: i32,
    pub status: UploadStatus,
    pub storage_backend: String,
    pub target_path: Option<String>, // Custom path for the file (e.g., "videos/2024/")
    pub final_path: Option<String>,
    pub checksum_sha256: Option<String>,
    pub webhook_url: Option<String>,
    pub finalization_started_at: Option<i64>,
    pub finalization_updated_at: Option<i64>,
    pub finalization_error: Option<String>,
    pub finalizing_progress_percent: i32,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum UploadStatus {
    Pending,
    Finalizing,
    Complete,
    Failed,
}

impl From<String> for UploadStatus {
    fn from(s: String) -> Self {
        match s.to_lowercase().as_str() {
            "finalizing" => UploadStatus::Finalizing,
            "complete" => UploadStatus::Complete,
            "failed" => UploadStatus::Failed,
            _ => UploadStatus::Pending,
        }
    }
}

impl std::fmt::Display for UploadStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadStatus::Pending => write!(f, "pending"),
            UploadStatus::Finalizing => write!(f, "finalizing"),
            UploadStatus::Complete => write!(f, "complete"),
            UploadStatus::Failed => write!(f, "failed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadPart {
    pub upload_id: String,
    pub part_number: i32,
    pub token_hash: String,
    pub status: PartStatus,
    pub size: i64,
    pub checksum_sha256: Option<String>,
    pub uploaded_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PartStatus {
    Pending,
    Uploaded,
}

impl From<String> for PartStatus {
    fn from(s: String) -> Self {
        match s.to_lowercase().as_str() {
            "uploaded" => PartStatus::Uploaded,
            _ => PartStatus::Pending,
        }
    }
}

impl std::fmt::Display for PartStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PartStatus::Pending => write!(f, "pending"),
            PartStatus::Uploaded => write!(f, "uploaded"),
        }
    }
}
