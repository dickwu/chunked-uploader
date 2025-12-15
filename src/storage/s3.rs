#![cfg(feature = "s3")]

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_s3::{config::Region, primitives::ByteStream, Client};
use bytes::Bytes;

use super::StorageBackend;
use crate::config::Config;
use crate::error::{AppError, Result};

pub struct S3Storage {
    client: Client,
    bucket: String,
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

        tracing::info!("S3Storage initialized for bucket: {}", config.s3_bucket);

        Ok(Self {
            client,
            bucket: config.s3_bucket.clone(),
        })
    }

    fn get_part_key(&self, upload_id: &str, part_number: i32) -> String {
        format!("parts/{}/part_{:06}", upload_id, part_number)
    }

    fn get_final_key(&self, upload_id: &str, filename: &str) -> String {
        // Sanitize filename
        let safe_filename = filename
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
            .collect::<String>();

        format!("files/{}_{}", upload_id, safe_filename)
    }
}

#[async_trait]
impl StorageBackend for S3Storage {
    async fn store_part(
        &self,
        upload_id: &str,
        part_number: i32,
        data: Bytes,
    ) -> Result<String> {
        let key = self.get_part_key(upload_id, part_number);

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(data.to_vec()))
            .send()
            .await
            .map_err(|e| AppError::Storage(format!("Failed to upload part to S3: {}", e)))?;

        tracing::debug!("Stored part {} for upload {} to S3", part_number, upload_id);

        Ok(key)
    }

    async fn read_part(&self, upload_id: &str, part_number: i32) -> Result<Bytes> {
        let key = self.get_part_key(upload_id, part_number);

        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| AppError::Storage(format!("Failed to read part from S3: {}", e)))?;

        let data = response
            .body
            .collect()
            .await
            .map_err(|e| AppError::Storage(format!("Failed to read S3 body: {}", e)))?;

        Ok(data.into_bytes())
    }

    async fn assemble_parts(
        &self,
        upload_id: &str,
        filename: &str,
        total_parts: i32,
    ) -> Result<String> {
        let final_key = self.get_final_key(upload_id, filename);

        // For S3, we need to use multipart upload to combine parts
        // First, initiate a multipart upload
        let create_response = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(&final_key)
            .send()
            .await
            .map_err(|e| AppError::Storage(format!("Failed to initiate multipart upload: {}", e)))?;

        let multipart_upload_id = create_response
            .upload_id()
            .ok_or_else(|| AppError::Storage("No upload ID returned from S3".to_string()))?;

        let mut completed_parts = Vec::new();

        // Copy each part to the multipart upload
        for part_num in 0..total_parts {
            let source_key = self.get_part_key(upload_id, part_num);
            let copy_source = format!("{}/{}", self.bucket, source_key);

            let upload_response = self
                .client
                .upload_part_copy()
                .bucket(&self.bucket)
                .key(&final_key)
                .upload_id(multipart_upload_id)
                .part_number((part_num + 1) as i32) // S3 part numbers are 1-indexed
                .copy_source(&copy_source)
                .send()
                .await
                .map_err(|e| {
                    AppError::Storage(format!("Failed to copy part {} to multipart: {}", part_num, e))
                })?;

            if let Some(copy_result) = upload_response.copy_part_result() {
                if let Some(etag) = copy_result.e_tag() {
                    completed_parts.push(
                        aws_sdk_s3::types::CompletedPart::builder()
                            .e_tag(etag)
                            .part_number((part_num + 1) as i32)
                            .build(),
                    );
                }
            }

            tracing::debug!("Copied part {} to multipart upload", part_num);
        }

        // Complete the multipart upload
        let completed_upload = aws_sdk_s3::types::CompletedMultipartUpload::builder()
            .set_parts(Some(completed_parts))
            .build();

        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(&final_key)
            .upload_id(multipart_upload_id)
            .multipart_upload(completed_upload)
            .send()
            .await
            .map_err(|e| AppError::Storage(format!("Failed to complete multipart upload: {}", e)))?;

        // Clean up source parts
        self.delete_parts(upload_id).await?;

        tracing::info!(
            "Assembled {} parts into s3://{}/{} for upload {}",
            total_parts,
            self.bucket,
            final_key,
            upload_id
        );

        Ok(format!("s3://{}/{}", self.bucket, final_key))
    }

    async fn delete_parts(&self, upload_id: &str) -> Result<()> {
        // List all parts for this upload
        let prefix = format!("parts/{}/", upload_id);

        let list_response = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&prefix)
            .send()
            .await
            .map_err(|e| AppError::Storage(format!("Failed to list parts: {}", e)))?;

        if let Some(contents) = list_response.contents() {
            for object in contents {
                if let Some(key) = object.key() {
                    self.client
                        .delete_object()
                        .bucket(&self.bucket)
                        .key(key)
                        .send()
                        .await
                        .map_err(|e| {
                            AppError::Storage(format!("Failed to delete part {}: {}", key, e))
                        })?;
                }
            }
        }

        tracing::debug!("Deleted all parts for upload {} from S3", upload_id);
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
