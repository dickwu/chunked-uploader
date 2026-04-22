use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::db::Database;
use crate::storage::StorageBackend;

pub struct CleanupService {
    db: Arc<Database>,
    storage: Arc<dyn StorageBackend>,
    config: Arc<Config>,
}

impl CleanupService {
    pub fn new(db: Arc<Database>, storage: Arc<dyn StorageBackend>, config: Arc<Config>) -> Self {
        Self {
            db,
            storage,
            config,
        }
    }

    /// Run the cleanup service indefinitely
    pub async fn run(&self) {
        let interval = Duration::from_secs(3600); // Run every hour
        tracing::info!(
            "Cleanup service started. TTL: {} hours, interval: {:?}",
            self.config.upload_ttl_hours,
            interval
        );

        loop {
            tokio::time::sleep(interval).await;
            self.cleanup_expired().await;
        }
    }

    /// Perform a single cleanup pass
    pub async fn cleanup_expired(&self) {
        tracing::debug!("Running cleanup for expired uploads...");

        match self.db.list_expired_pending_uploads() {
            Ok(expired_uploads) => {
                for upload in expired_uploads {
                    if let Err(e) = self.storage.cleanup_incomplete_upload(&upload).await {
                        tracing::warn!(
                            "Failed to cleanup storage for expired upload {}: {}",
                            upload.id,
                            e
                        );
                    }

                    if let Err(e) = self.db.delete_upload(&upload.id) {
                        tracing::warn!(
                            "Failed to delete expired upload {} from database: {}",
                            upload.id,
                            e
                        );
                        continue;
                    }

                    tracing::info!("Cleaned up expired upload: {}", upload.id);
                }
            }
            Err(e) => {
                tracing::error!("Failed to query expired uploads: {}", e);
            }
        }
    }
}
