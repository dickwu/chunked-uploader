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
    pub fn new(
        db: Arc<Database>,
        storage: Arc<dyn StorageBackend>,
        config: Arc<Config>,
    ) -> Self {
        Self { db, storage, config }
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

        match self.db.delete_expired_uploads() {
            Ok(expired_ids) => {
                for upload_id in expired_ids {
                    // Clean up storage for each expired upload
                    if let Err(e) = self.storage.delete_parts(&upload_id).await {
                        tracing::warn!(
                            "Failed to delete parts for expired upload {}: {}",
                            upload_id,
                            e
                        );
                    }
                    tracing::info!("Cleaned up expired upload: {}", upload_id);
                }
            }
            Err(e) => {
                tracing::error!("Failed to query expired uploads: {}", e);
            }
        }
    }
}

