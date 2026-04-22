use async_trait::async_trait;
use bytes::Bytes;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::StorageBackend;
use crate::db::schema::Upload;
use crate::error::{AppError, Result};

pub struct LocalStorage {
    // Temp storage for parts (local SSD - fast I/O)
    parts_path: PathBuf,
    // Final storage for assembled files
    final_path: PathBuf,
}

impl LocalStorage {
    pub fn new(final_storage_path: &str, temp_storage_path: &str) -> Result<Self> {
        let parts_path = PathBuf::from(temp_storage_path).join("parts");
        let final_path = PathBuf::from(final_storage_path);

        tracing::info!("Initializing LocalStorage...");
        tracing::info!("  Parts: {}", parts_path.display());
        tracing::info!("  Final storage: {}", final_path.display());

        // Create directories
        std::fs::create_dir_all(&parts_path)
            .map_err(|e| AppError::Storage(format!("Failed to create parts directory: {}", e)))?;

        std::fs::create_dir_all(&final_path)
            .map_err(|e| AppError::Storage(format!("Failed to create files directory: {}", e)))?;

        // Test write permissions (only for local paths - these won't hang)
        Self::verify_write_permission(&parts_path, "parts")?;
        Self::verify_write_permission(&final_path, "final")?;

        tracing::info!("LocalStorage initialized:");
        tracing::info!("  ✓ Parts: {}", parts_path.display());
        tracing::info!("  ✓ Files: {}", final_path.display());

        Ok(Self {
            parts_path,
            final_path,
        })
    }

    /// Verify write permission by creating and deleting a test file
    fn verify_write_permission(path: &PathBuf, name: &str) -> Result<()> {
        use std::io::Write;

        let test_file = path.join(".write_test");

        let mut file = std::fs::File::create(&test_file).map_err(|e| {
            AppError::Storage(format!(
                "No write permission for {} directory {}: {}",
                name,
                path.display(),
                e
            ))
        })?;

        file.write_all(b"test").map_err(|e| {
            AppError::Storage(format!("Failed write test in {} directory: {}", name, e))
        })?;

        drop(file);
        std::fs::remove_file(&test_file).ok();

        Ok(())
    }

    fn get_part_path(&self, upload_id: &str, part_number: i32) -> PathBuf {
        self.parts_path
            .join(upload_id)
            .join(format!("part_{:06}", part_number))
    }

    fn get_upload_parts_dir(&self, upload_id: &str) -> PathBuf {
        self.parts_path.join(upload_id)
    }

    fn get_final_file_path(
        &self,
        upload_id: &str,
        filename: &str,
        target_path: Option<&str>,
    ) -> PathBuf {
        // Sanitize filename to prevent path traversal
        let safe_filename = Path::new(filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed");

        let final_filename = format!("{}_{}", upload_id, safe_filename);

        // Use custom path if provided
        match target_path {
            Some(path) => {
                // Sanitize path (remove leading slashes, prevent traversal)
                let clean_path: String = path
                    .trim_matches('/')
                    .chars()
                    .filter(|c| {
                        c.is_alphanumeric() || *c == '/' || *c == '.' || *c == '-' || *c == '_'
                    })
                    .collect();

                if clean_path.is_empty() {
                    self.final_path.join(&final_filename)
                } else {
                    self.final_path.join(&clean_path).join(&final_filename)
                }
            }
            None => self.final_path.join(&final_filename),
        }
    }

    fn get_assembled_temp_path(final_path: &Path) -> PathBuf {
        let file_name = final_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("assembled");

        final_path.with_file_name(format!(".{}.assembling", file_name))
    }
}

#[async_trait]
impl StorageBackend for LocalStorage {
    async fn store_part(&self, upload: &Upload, part_number: i32, data: Bytes) -> Result<String> {
        let upload_id = &upload.id;
        let part_path = self.get_part_path(upload_id, part_number);
        let data_len = data.len();

        // Parts go to local temp storage (fast SSD) - use normal async I/O
        if let Some(parent) = part_path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                AppError::Storage(format!("Failed to create upload directory: {}", e))
            })?;
        }

        let mut file = fs::File::create(&part_path)
            .await
            .map_err(|e| AppError::Storage(format!("Failed to create part file: {}", e)))?;

        file.write_all(&data)
            .await
            .map_err(|e| AppError::Storage(format!("Failed to write part data: {}", e)))?;

        file.flush()
            .await
            .map_err(|e| AppError::Storage(format!("Failed to flush part data: {}", e)))?;

        tracing::debug!(
            "Stored part {} for upload {} ({} bytes) at {:?}",
            part_number,
            upload_id,
            data_len,
            part_path
        );

        Ok(part_path.to_string_lossy().to_string())
    }

    async fn read_part(&self, upload_id: &str, part_number: i32) -> Result<Bytes> {
        let part_path = self.get_part_path(upload_id, part_number);

        // Parts are on local temp storage - use async I/O
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
        let final_path = self.get_final_file_path(upload_id, filename, target_path);
        let parts_path = self.parts_path.clone();
        let upload_id_owned = upload_id.to_string();

        // Create target directory if needed
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                AppError::Storage(format!("Failed to create target directory: {}", e))
            })?;
        }

        let temp_assembled = Self::get_assembled_temp_path(&final_path);
        if temp_assembled.exists() {
            fs::remove_file(&temp_assembled).await.map_err(|e| {
                AppError::Storage(format!(
                    "Failed to remove stale temp assembled file {}: {}",
                    temp_assembled.display(),
                    e
                ))
            })?;
        }

        tracing::info!(
            "Assembling {} parts for upload {} to {:?} via staging file {:?}",
            total_parts,
            upload_id,
            final_path,
            temp_assembled
        );

        // Assemble to a staging file beside the destination so the final rename stays on one device.
        let mut temp_file = fs::File::create(&temp_assembled).await.map_err(|e| {
            AppError::Storage(format!("Failed to create temp assembled file: {}", e))
        })?;

        for part_num in 0..total_parts {
            let part_path = parts_path
                .join(&upload_id_owned)
                .join(format!("part_{:06}", part_num));

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

        // Move the staged file into place.
        fs::rename(&temp_assembled, &final_path)
            .await
            .map_err(|e| AppError::Storage(format!("Failed to move assembled file: {}", e)))?;

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
            // Parts are on local temp storage - use async I/O
            fs::remove_dir_all(&parts_dir).await.map_err(|e| {
                AppError::Storage(format!("Failed to delete parts directory: {}", e))
            })?;
            tracing::debug!("Deleted parts directory for upload {}", upload_id);
        }

        Ok(())
    }

    async fn delete_file(&self, path: &str) -> Result<()> {
        let file_path = PathBuf::from(path);

        if file_path.exists() {
            // All files are local - use async I/O
            fs::remove_file(&file_path)
                .await
                .map_err(|e| AppError::Storage(format!("Failed to delete file {}: {}", path, e)))?;
            tracing::debug!("Deleted file {:?}", file_path);
        }

        Ok(())
    }

    fn backend_type(&self) -> &'static str {
        "local"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::{Upload, UploadStatus};
    use bytes::Bytes;
    use uuid::Uuid;

    fn test_upload(upload_id: &str, target_path: Option<&str>) -> Upload {
        Upload {
            id: upload_id.to_string(),
            filename: "clip.mp4".to_string(),
            total_size: 11,
            chunk_size: 6,
            total_parts: 2,
            status: UploadStatus::Pending,
            storage_backend: "local".to_string(),
            target_path: target_path.map(str::to_string),
            final_path: None,
            checksum_sha256: None,
            webhook_url: None,
            finalization_started_at: None,
            finalization_updated_at: None,
            finalization_error: None,
            finalizing_progress_percent: 0,
            created_at: 0,
            updated_at: 0,
            expires_at: 0,
        }
    }

    fn test_root() -> PathBuf {
        std::env::temp_dir().join(format!("chunked-uploader-local-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn assemble_parts_uses_destination_staging_file() {
        let root = test_root();
        let final_root = root.join("final");
        let temp_root = root.join("temp");

        let storage =
            LocalStorage::new(final_root.to_str().unwrap(), temp_root.to_str().unwrap()).unwrap();
        let upload = test_upload("upload-123", Some("sermons/2026"));

        let expected_final = storage.get_final_file_path(
            &upload.id,
            &upload.filename,
            upload.target_path.as_deref(),
        );
        let staged_final = LocalStorage::get_assembled_temp_path(&expected_final);

        assert_eq!(staged_final.parent(), expected_final.parent());
        assert_ne!(staged_final.parent(), Some(storage.parts_path.as_path()));

        storage
            .store_part(&upload, 0, Bytes::from_static(b"hello "))
            .await
            .unwrap();
        storage
            .store_part(&upload, 1, Bytes::from_static(b"world"))
            .await
            .unwrap();

        let final_path = storage
            .assemble_parts(
                &upload.id,
                &upload.filename,
                upload.total_parts,
                upload.target_path.as_deref(),
            )
            .await
            .unwrap();

        assert_eq!(final_path, expected_final.to_string_lossy());
        assert_eq!(fs::read(&expected_final).await.unwrap(), b"hello world");
        assert!(!staged_final.exists());
        assert!(!storage.get_upload_parts_dir(&upload.id).exists());

        let _ = fs::remove_dir_all(&root).await;
    }
}
