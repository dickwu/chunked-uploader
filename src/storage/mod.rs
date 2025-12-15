pub mod local;

#[cfg(feature = "s3")]
pub mod s3;

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::Result;

#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// Store a chunk/part of an upload
    async fn store_part(
        &self,
        upload_id: &str,
        part_number: i32,
        data: Bytes,
    ) -> Result<String>;

    /// Read a part back (for assembly or verification)
    async fn read_part(&self, upload_id: &str, part_number: i32) -> Result<Bytes>;

    /// Assemble all parts into a final file
    async fn assemble_parts(
        &self,
        upload_id: &str,
        filename: &str,
        total_parts: i32,
    ) -> Result<String>;

    /// Delete all parts for an upload
    async fn delete_parts(&self, upload_id: &str) -> Result<()>;

    /// Delete a single completed file
    async fn delete_file(&self, path: &str) -> Result<()>;

    /// Get the backend type name
    fn backend_type(&self) -> &'static str;
}
