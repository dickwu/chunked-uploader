pub mod local;

#[cfg(feature = "smb")]
pub mod smb;

#[cfg(feature = "s3")]
pub mod s3;

use async_trait::async_trait;
use bytes::Bytes;
use std::path::Path;

use crate::db::schema::Upload;
use crate::error::Result;

/// Sanitize a target path: strip leading/trailing slashes, allow only safe characters.
/// Shared across all storage backends.
pub fn sanitize_target_path(path: &str) -> String {
    path.trim_matches('/')
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '/' || *c == '.' || *c == '-' || *c == '_')
        .collect()
}

/// Build a clean, backend-agnostic response path for the client.
/// Returns relative paths like `videos/2024/uuid_movie.mp4` or `files/uuid_movie.mp4`.
pub fn build_response_path(upload_id: &str, filename: &str, target_path: Option<&str>) -> String {
    let safe_filename = Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed");

    let final_filename = format!("{}_{}", upload_id, safe_filename);

    match target_path {
        Some(path) => {
            let clean_path = sanitize_target_path(path);
            if clean_path.is_empty() {
                final_filename
            } else {
                format!("{}/{}", clean_path, final_filename)
            }
        }
        None => format!("files/{}", final_filename),
    }
}

#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// Store a chunk/part of an upload
    async fn store_part(
        &self,
        upload: &Upload,
        part_number: i32,
        data: Bytes,
    ) -> Result<String>;

    /// Read a part back (for assembly or verification)
    async fn read_part(&self, upload_id: &str, part_number: i32) -> Result<Bytes>;

    /// Assemble all parts into a final file
    /// target_path is an optional custom path prefix (e.g., "videos/2024/")
    async fn assemble_parts(
        &self,
        upload_id: &str,
        filename: &str,
        total_parts: i32,
        target_path: Option<&str>,
    ) -> Result<String>;

    /// Validate upload artifacts are ready for finalization.
    async fn verify_upload_ready(&self, _upload: &Upload) -> Result<()> {
        Ok(())
    }

    /// Finalize upload after all parts are uploaded and verified.
    async fn finalize_upload(&self, upload: &Upload) -> Result<String> {
        self.assemble_parts(
            &upload.id,
            &upload.filename,
            upload.total_parts,
            upload.target_path.as_deref(),
        )
        .await
    }

    /// Cleanup incomplete upload artifacts.
    async fn cleanup_incomplete_upload(&self, upload: &Upload) -> Result<()> {
        self.delete_parts(&upload.id).await
    }

    /// Delete all parts for an upload
    async fn delete_parts(&self, upload_id: &str) -> Result<()>;

    /// Delete a single completed file
    async fn delete_file(&self, path: &str) -> Result<()>;

    /// Get the backend type name
    fn backend_type(&self) -> &'static str;
    
    /// Check if backend is healthy (connected and writable)
    /// Returns (is_healthy, optional_message)
    async fn health_check(&self) -> (bool, Option<String>) {
        // Default implementation assumes healthy
        (true, None)
    }
}
