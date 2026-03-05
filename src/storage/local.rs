use async_trait::async_trait;
use bytes::Bytes;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::{build_response_path, sanitize_target_path, StorageBackend};
use crate::db::schema::Upload;
use crate::error::{AppError, Result};

pub struct LocalStorage {
    // Temp storage for parts (local SSD - fast I/O)
    parts_path: PathBuf,
    // Final storage for assembled files (can be NAS/network)
    final_path: PathBuf,
}

impl LocalStorage {
    pub fn new(_final_storage_path: &str, temp_storage_path: &str) -> Result<Self> {
        // Use ONLY local temp storage for everything
        // NAS/SMB mounts have severe issues with Rust's file I/O on macOS
        let base_path = PathBuf::from(temp_storage_path);
        let parts_path = base_path.join("parts");
        let final_path = base_path.join("files");

        tracing::info!("Initializing LocalStorage...");
        tracing::info!("  Storage base: {}", base_path.display());
        tracing::warn!("  NOTE: Using local storage only. NAS path ignored due to macOS SMB issues.");
        tracing::warn!("  Files will be stored at: {}", final_path.display());

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
    fn verify_write_permission(path: &Path, name: &str) -> Result<()> {
        use std::io::Write;

        let test_file = path.join(".write_test");

        let mut file = std::fs::File::create(&test_file).map_err(|e| {
            AppError::Storage(format!(
                "No write permission for {} directory {}: {}",
                name, path.display(), e
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

    fn get_final_file_path(&self, upload_id: &str, filename: &str, target_path: Option<&str>) -> PathBuf {
        let safe_filename = Path::new(filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed");

        let final_filename = format!("{}_{}", upload_id, safe_filename);

        match target_path {
            Some(path) => {
                let clean_path = sanitize_target_path(path);
                if clean_path.is_empty() {
                    self.final_path.join(&final_filename)
                } else {
                    self.final_path.join(&clean_path).join(&final_filename)
                }
            }
            None => self.final_path.join(&final_filename),
        }
    }

    /// Resolve a client-facing response path back to an absolute filesystem path.
    fn resolve_response_path(&self, path: &str) -> PathBuf {
        if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else if let Some(rest) = path.strip_prefix("files/") {
            self.final_path.join(rest)
        } else {
            self.final_path.join(path)
        }
    }
}

#[async_trait]
impl StorageBackend for LocalStorage {
    async fn store_part(
        &self,
        upload: &Upload,
        part_number: i32,
        data: Bytes,
    ) -> Result<String> {
        let upload_id = &upload.id;
        let part_path = self.get_part_path(upload_id, part_number);
        let data_len = data.len();

        // Parts go to local temp storage (fast SSD) - use normal async I/O
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

        tracing::info!(
            "Assembling {} parts for upload {} to {:?}",
            total_parts,
            upload_id,
            final_path
        );

        // Assemble to temp file first (local), then move to final destination
        let temp_assembled = self.parts_path.join(format!("{}_assembled.tmp", upload_id));
        
        // Use async I/O for reading parts (from local temp storage)
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

        // Move assembled file to final location (both are on local storage)
        fs::rename(&temp_assembled, &final_path).await.map_err(|e| {
            AppError::Storage(format!("Failed to move assembled file: {}", e))
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

    async fn verify_upload_ready(&self, upload: &Upload) -> Result<()> {
        let upload_id = &upload.id;
        let mut total_bytes: u64 = 0;

        for part_num in 0..upload.total_parts {
            let part_path = self.get_part_path(upload_id, part_num);
            let metadata = fs::metadata(&part_path).await.map_err(|e| {
                AppError::Storage(format!(
                    "Verification failed: part {} missing for upload {}: {}",
                    part_num, upload_id, e
                ))
            })?;
            total_bytes += metadata.len();
        }

        let expected = upload.total_size as u64;
        if total_bytes != expected {
            return Err(AppError::Storage(format!(
                "Verification failed: size mismatch for upload {} (expected {}, got {})",
                upload_id, expected, total_bytes
            )));
        }

        Ok(())
    }

    async fn finalize_upload(&self, upload: &Upload) -> Result<String> {
        self.assemble_parts(
            &upload.id,
            &upload.filename,
            upload.total_parts,
            upload.target_path.as_deref(),
        )
        .await?;

        Ok(build_response_path(
            &upload.id,
            &upload.filename,
            upload.target_path.as_deref(),
        ))
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
        let file_path = self.resolve_response_path(path);

        if file_path.exists() {
            fs::remove_file(&file_path).await.map_err(|e| {
                AppError::Storage(format!("Failed to delete file {}: {}", path, e))
            })?;
            tracing::debug!("Deleted file {:?}", file_path);
        }

        Ok(())
    }

    fn backend_type(&self) -> &'static str {
        "local"
    }
}
