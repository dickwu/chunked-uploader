pub mod local;

#[cfg(feature = "smb")]
pub mod smb;

#[cfg(feature = "s3")]
pub mod s3;

use async_trait::async_trait;
use bytes::Bytes;

use crate::db::schema::Upload;
use crate::error::Result;

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
