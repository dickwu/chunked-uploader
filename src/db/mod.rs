pub mod repository;
pub mod schema;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;

use crate::error::{AppError, Result};

pub type DbPool = Pool<SqliteConnectionManager>;

pub struct Database {
    pool: DbPool,
}

impl Database {
    pub fn new(path: &str) -> Result<Self> {
        let manager = SqliteConnectionManager::file(path);
        let pool = Pool::builder()
            .max_size(10)
            .build(manager)
            .map_err(|e| AppError::Internal(format!("Failed to create database pool: {}", e)))?;

        Ok(Database { pool })
    }

    pub fn run_migrations(&self) -> Result<()> {
        let conn = self.pool.get().map_err(|e| {
            AppError::Internal(format!("Failed to get database connection: {}", e))
        })?;

        // Create uploads table
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS uploads (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                total_size INTEGER NOT NULL,
                chunk_size INTEGER NOT NULL,
                total_parts INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                storage_backend TEXT NOT NULL,
                target_path TEXT,
                final_path TEXT,
                checksum_sha256 TEXT,
                webhook_url TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            )
            "#,
            [],
        )?;

        // Add columns if they don't exist (migrations for existing DBs)
        let _ = conn.execute("ALTER TABLE uploads ADD COLUMN webhook_url TEXT", []);
        let _ = conn.execute("ALTER TABLE uploads ADD COLUMN target_path TEXT", []);

        // Create upload_parts table
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS upload_parts (
                upload_id TEXT NOT NULL,
                part_number INTEGER NOT NULL,
                token_hash TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                size INTEGER NOT NULL,
                checksum_sha256 TEXT,
                uploaded_at INTEGER,
                PRIMARY KEY (upload_id, part_number),
                FOREIGN KEY (upload_id) REFERENCES uploads(id) ON DELETE CASCADE
            )
            "#,
            [],
        )?;

        // Create indexes
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_uploads_status ON uploads(status)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_uploads_expires_at ON uploads(expires_at)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_upload_parts_status ON upload_parts(status)",
            [],
        )?;

        tracing::info!("Database migrations completed");
        Ok(())
    }

    pub fn get_conn(
        &self,
    ) -> Result<r2d2::PooledConnection<SqliteConnectionManager>> {
        self.pool
            .get()
            .map_err(|e| AppError::Internal(format!("Failed to get database connection: {}", e)))
    }

    /// Delete expired uploads and return their IDs for cleanup
    pub fn delete_expired_uploads(&self) -> Result<Vec<String>> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().timestamp();

        // First get the IDs of expired uploads
        let mut stmt = conn.prepare(
            "SELECT id FROM uploads WHERE expires_at < ?1 AND status = 'pending'",
        )?;
        let expired_ids: Vec<String> = stmt
            .query_map(params![now], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        if !expired_ids.is_empty() {
            // Delete the parts first (foreign key)
            for id in &expired_ids {
                conn.execute("DELETE FROM upload_parts WHERE upload_id = ?1", params![id])?;
            }

            // Then delete the uploads
            conn.execute(
                "DELETE FROM uploads WHERE expires_at < ?1 AND status = 'pending'",
                params![now],
            )?;

            tracing::info!("Deleted {} expired uploads", expired_ids.len());
        }

        Ok(expired_ids)
    }
}

