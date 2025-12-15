use async_trait::async_trait;
use bytes::Bytes;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::StorageBackend;
use crate::error::{AppError, Result};

pub struct LocalStorage {
    base_path: PathBuf,
    parts_path: PathBuf,
    final_path: PathBuf,
}

impl LocalStorage {
    pub fn new(base_path: &str) -> Result<Self> {
        let base_path = PathBuf::from(base_path);
        let parts_path = base_path.join("parts");
        let final_path = base_path.join("files");

        // Create directories synchronously during initialization
        std::fs::create_dir_all(&parts_path)
            .map_err(|e| AppError::Storage(format!("Failed to create parts directory: {}", e)))?;
        std::fs::create_dir_all(&final_path)
            .map_err(|e| AppError::Storage(format!("Failed to create files directory: {}", e)))?;

        tracing::info!("LocalStorage initialized at {:?}", base_path);

        Ok(Self {
            base_path,
            parts_path,
            final_path,
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

    fn get_final_file_path(&self, upload_id: &str, filename: &str) -> PathBuf {
        // Sanitize filename to prevent path traversal
        let safe_filename = Path::new(filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed");

        self.final_path.join(format!("{}_{}", upload_id, safe_filename))
    }
}

#[async_trait]
impl StorageBackend for LocalStorage {
    async fn store_part(
        &self,
        upload_id: &str,
        part_number: i32,
        data: Bytes,
    ) -> Result<String> {
        let part_path = self.get_part_path(upload_id, part_number);

        // Ensure upload directory exists
        if let Some(parent) = part_path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                AppError::Storage(format!("Failed to create upload directory: {}", e))
            })?;
        }

        // Write part data
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
            "Stored part {} for upload {} ({} bytes)",
            part_number,
            upload_id,
            data.len()
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
    ) -> Result<String> {
        let final_path = self.get_final_file_path(upload_id, filename);

        // Create final file
        let mut final_file = fs::File::create(&final_path).await.map_err(|e| {
            AppError::Storage(format!("Failed to create final file: {}", e))
        })?;

        // Assemble parts in order
        for part_num in 0..total_parts {
            let part_path = self.get_part_path(upload_id, part_num);

            // Read part and append to final file
            let part_data = fs::read(&part_path).await.map_err(|e| {
                AppError::Storage(format!(
                    "Failed to read part {} during assembly: {}",
                    part_num, e
                ))
            })?;

            final_file.write_all(&part_data).await.map_err(|e| {
                AppError::Storage(format!("Failed to write to final file: {}", e))
            })?;

            tracing::debug!("Assembled part {} ({} bytes)", part_num, part_data.len());
        }

        final_file.flush().await.map_err(|e| {
            AppError::Storage(format!("Failed to flush final file: {}", e))
        })?;

        // Clean up parts directory
        self.delete_parts(upload_id).await?;

        tracing::info!(
            "Assembled {} parts into {:?} for upload {}",
            total_parts,
            final_path,
            upload_id
        );

        Ok(final_path.to_string_lossy().to_string())
    }

    async fn delete_parts(&self, upload_id: &str) -> Result<()> {
        let parts_dir = self.get_upload_parts_dir(upload_id);

        if parts_dir.exists() {
            fs::remove_dir_all(&parts_dir).await.map_err(|e| {
                AppError::Storage(format!("Failed to delete parts directory: {}", e))
            })?;
            tracing::debug!("Deleted parts directory for upload {}", upload_id);
        }

        Ok(())
    }

    async fn delete_file(&self, path: &str) -> Result<()> {
        let file_path = Path::new(path);

        if file_path.exists() {
            fs::remove_file(file_path).await.map_err(|e| {
                AppError::Storage(format!("Failed to delete file: {}", e))
            })?;
            tracing::debug!("Deleted file {:?}", file_path);
        }

        Ok(())
    }

    fn backend_type(&self) -> &'static str {
        "local"
    }
}

