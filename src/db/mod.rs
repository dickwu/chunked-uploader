pub mod repository;
pub mod schema;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;

use crate::db::schema::Upload;

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
                finalization_started_at INTEGER,
                finalization_updated_at INTEGER,
                finalization_error TEXT,
                finalizing_progress_percent INTEGER NOT NULL DEFAULT 0,
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
        let _ = conn.execute("ALTER TABLE uploads ADD COLUMN finalization_started_at INTEGER", []);
        let _ = conn.execute("ALTER TABLE uploads ADD COLUMN finalization_updated_at INTEGER", []);
        let _ = conn.execute("ALTER TABLE uploads ADD COLUMN finalization_error TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE uploads ADD COLUMN finalizing_progress_percent INTEGER NOT NULL DEFAULT 0",
            [],
        );

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

    /// List expired pending uploads for cleanup.
    pub fn list_expired_pending_uploads(&self) -> Result<Vec<Upload>> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().timestamp();

        let mut stmt = conn.prepare(
            r#"
            SELECT id, filename, total_size, chunk_size, total_parts,
                   status, storage_backend, target_path, final_path, checksum_sha256,
                   webhook_url, finalization_started_at, finalization_updated_at,
                   finalization_error, finalizing_progress_percent,
                   created_at, updated_at, expires_at
            FROM uploads
            WHERE expires_at < ?1 AND status = 'pending'
            "#,
        )?;
        let uploads: Vec<Upload> = stmt
            .query_map(params![now], |row| {
                Ok(Upload {
                    id: row.get(0)?,
                    filename: row.get(1)?,
                    total_size: row.get(2)?,
                    chunk_size: row.get(3)?,
                    total_parts: row.get(4)?,
                    status: row.get::<_, String>(5)?.into(),
                    storage_backend: row.get(6)?,
                    target_path: row.get(7)?,
                    final_path: row.get(8)?,
                    checksum_sha256: row.get(9)?,
                    webhook_url: row.get(10)?,
                    finalization_started_at: row.get(11)?,
                    finalization_updated_at: row.get(12)?,
                    finalization_error: row.get(13)?,
                    finalizing_progress_percent: row.get(14)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                    expires_at: row.get(17)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(uploads)
    }

    /// Mark finalizing uploads as failed on startup recovery.
    pub fn mark_stale_finalizing_failed_on_boot(&self) -> Result<usize> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().timestamp();
        let affected = conn.execute(
            r#"
            UPDATE uploads
            SET status = 'failed',
                finalization_error = COALESCE(finalization_error, 'Server restarted during finalization'),
                finalization_updated_at = ?1,
                updated_at = ?1
            WHERE status = 'finalizing'
            "#,
            params![now],
        )?;
        Ok(affected)
    }
}
