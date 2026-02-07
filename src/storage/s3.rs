//! S3 Storage Backend
//!
//! Uses local fast storage for parts, then uploads final assembled file to S3.
//! This avoids S3 multipart upload complexity and minimum part size restrictions.

#![cfg(feature = "s3")]

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_s3::{config::Region, primitives::ByteStream, Client};
use bytes::Bytes;
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::StorageBackend;
use crate::config::Config;
use crate::db::schema::Upload;
use crate::error::{AppError, Result};

pub struct S3Storage {
    client: Client,
    bucket: String,
    /// Local temp storage for parts (fast SSD)
    parts_path: PathBuf,
}

impl S3Storage {
    pub async fn new(config: &Config) -> Result<Self> {
        let region = Region::new(config.s3_region.clone());

        let mut aws_config = aws_config::defaults(BehaviorVersion::latest()).region(region);

        // Use custom endpoint if provided (for MinIO, etc.)
        if let Some(endpoint) = &config.s3_endpoint {
            aws_config = aws_config.endpoint_url(endpoint);
        }

        let sdk_config = aws_config.load().await;
        let client = Client::new(&sdk_config);

        // Use temp storage path for parts (like SMB does)
        let parts_path = PathBuf::from(&config.temp_storage_path).join("parts");
        
        // Create local parts directory
        std::fs::create_dir_all(&parts_path)
            .map_err(|e| AppError::Storage(format!("Failed to create parts directory: {}", e)))?;

        tracing::info!("S3Storage initialized:");
        tracing::info!("  Bucket: {}", config.s3_bucket);
        tracing::info!("  Region: {}", config.s3_region);
        if let Some(endpoint) = &config.s3_endpoint {
            tracing::info!("  Endpoint: {}", endpoint);
        }
        tracing::info!("  Local parts: {}", parts_path.display());

        Ok(Self {
            client,
            bucket: config.s3_bucket.clone(),
            parts_path,
        })
    }

    fn get_part_path(&self, upload_id: &str, part_number: i32) -> PathBuf {
        self.parts_path
            .join(upload_id)
            .join(format!("part_{:06}", part_number))
    }

    fn get_upload_parts_dir(&self, upload_id: &str) -> PathBuf {
        self.parts_path.join(upload_id)
    }

    fn get_final_key(&self, upload_id: &str, filename: &str, target_path: Option<&str>) -> String {
        // Sanitize filename
        let safe_filename = filename
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
            .collect::<String>();

        // Use custom path if provided, otherwise default to "files/"
        match target_path {
            Some(path) => {
                // Sanitize and normalize path (remove leading/trailing slashes)
                let clean_path = path
                    .trim_matches('/')
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '/' || *c == '.' || *c == '-' || *c == '_')
                    .collect::<String>();
                
                if clean_path.is_empty() {
                    format!("{}_{}", upload_id, safe_filename)
                } else {
                    format!("{}/{}_{}", clean_path, upload_id, safe_filename)
                }
            }
            None => format!("files/{}_{}", upload_id, safe_filename),
        }
    }
}

#[async_trait]
impl StorageBackend for S3Storage {
    async fn store_part(
        &self,
        upload: &Upload,
        part_number: i32,
        data: Bytes,
    ) -> Result<String> {
        let upload_id = &upload.id;
        let part_path = self.get_part_path(upload_id, part_number);
        let data_len = data.len();

        // Parts go to local temp storage - use async I/O
        if let Some(parent) = part_path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                AppError::Storage(format!("Failed to create upload directory: {}", e))
            })?;
        }

        let mut file = fs::File::create(&part_path).await.map_err(|e| {
            AppError::Storage(format!("Failed to create part file: {}", e))
        })?;

        file.write_all(&data).await.map_err(|e| {
            AppError::Storage(format!("Failed to write part data: {}", e))
        })?;

        file.flush().await.map_err(|e| {
            AppError::Storage(format!("Failed to flush part data: {}", e))
        })?;

        tracing::debug!(
            "Stored part {} for upload {} ({} bytes) locally",
            part_number,
            upload_id,
            data_len
        );

        Ok(part_path.to_string_lossy().to_string())
    }

    async fn read_part(&self, upload_id: &str, part_number: i32) -> Result<Bytes> {
        let part_path = self.get_part_path(upload_id, part_number);

        let data = fs::read(&part_path).await.map_err(|e| {
            AppError::Storage(format!(
                "Failed to read part {} for upload {}: {}",
                part_number, upload_id, e
            ))
        })?;

        Ok(Bytes::from(data))
    }

    async fn assemble_parts(
        &self,
        upload_id: &str,
        filename: &str,
        total_parts: i32,
        target_path: Option<&str>,
    ) -> Result<String> {
        let final_key = self.get_final_key(upload_id, filename, target_path);

        tracing::info!(
            "Assembling {} parts for upload {} to S3: {}",
            total_parts,
            upload_id,
            final_key
        );

        // Step 1: Assemble parts locally first
        let temp_assembled = self.parts_path.join(format!("{}_assembled.tmp", upload_id));

        let mut temp_file = fs::File::create(&temp_assembled).await.map_err(|e| {
            AppError::Storage(format!("Failed to create temp assembled file: {}", e))
        })?;

        for part_num in 0..total_parts {
            let part_path = self.get_part_path(upload_id, part_num);

            let part_data = fs::read(&part_path).await.map_err(|e| {
                AppError::Storage(format!(
                    "Failed to read part {} during assembly: {}",
                    part_num, e
                ))
            })?;

            temp_file.write_all(&part_data).await.map_err(|e| {
                AppError::Storage(format!("Failed to write to temp assembled file: {}", e))
            })?;

            tracing::debug!("Assembled part {} ({} bytes)", part_num, part_data.len());
        }

        temp_file.flush().await.map_err(|e| {
            AppError::Storage(format!("Failed to flush temp assembled file: {}", e))
        })?;
        drop(temp_file);

        // Step 2: Upload assembled file to S3 as a single object
        let file_size = std::fs::metadata(&temp_assembled)
            .map_err(|e| AppError::Storage(format!("Failed to get file size: {}", e)))?
            .len();

        tracing::info!(
            "Uploading assembled file to S3: {} ({} bytes)",
            final_key,
            file_size
        );

        let body = ByteStream::from_path(&temp_assembled)
            .await
            .map_err(|e| AppError::Storage(format!("Failed to read assembled file: {}", e)))?;

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&final_key)
            .body(body)
            .send()
            .await
            .map_err(|e| AppError::Storage(format!("Failed to upload to S3: {}", e)))?;

        // Step 3: Clean up local temp files
        fs::remove_file(&temp_assembled).await.ok();
        self.delete_parts(upload_id).await?;

        let final_path = format!("s3://{}/{}", self.bucket, final_key);
        tracing::info!(
            "Assembled {} parts into {} for upload {}",
            total_parts,
            final_path,
            upload_id
        );

        Ok(final_path)
    }

    async fn delete_parts(&self, upload_id: &str) -> Result<()> {
        let parts_dir = self.get_upload_parts_dir(upload_id);

        if parts_dir.exists() {
            fs::remove_dir_all(&parts_dir).await.map_err(|e| {
                AppError::Storage(format!("Failed to delete parts directory: {}", e))
            })?;
            tracing::debug!("Deleted local parts directory for upload {}", upload_id);
        }

        Ok(())
    }

    async fn delete_file(&self, path: &str) -> Result<()> {
        // Extract key from s3:// URL or use as-is
        let key = path
            .strip_prefix(&format!("s3://{}/", self.bucket))
            .unwrap_or(path);

        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| AppError::Storage(format!("Failed to delete S3 object: {}", e)))?;

        tracing::debug!("Deleted S3 object: {}", key);
        Ok(())
    }

    fn backend_type(&self) -> &'static str {
        "s3"
    }
}
