use rusqlite::params;

use super::schema::{PartStatus, Upload, UploadPart, UploadStatus};
use super::Database;
use crate::error::{AppError, Result};

impl Database {
    // ============ Upload Operations ============

    pub fn create_upload(&self, upload: &Upload) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"
            INSERT INTO uploads (
                id, filename, total_size, chunk_size, total_parts,
                status, storage_backend, target_path, final_path, checksum_sha256,
                webhook_url, finalization_started_at, finalization_updated_at,
                finalization_error, finalizing_progress_percent,
                created_at, updated_at, expires_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
            )
            "#,
            params![
                upload.id,
                upload.filename,
                upload.total_size,
                upload.chunk_size,
                upload.total_parts,
                upload.status.to_string(),
                upload.storage_backend,
                upload.target_path,
                upload.final_path,
                upload.checksum_sha256,
                upload.webhook_url,
                upload.finalization_started_at,
                upload.finalization_updated_at,
                upload.finalization_error,
                upload.finalizing_progress_percent,
                upload.created_at,
                upload.updated_at,
                upload.expires_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_upload(&self, id: &str) -> Result<Upload> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, filename, total_size, chunk_size, total_parts,
                   status, storage_backend, target_path, final_path, checksum_sha256,
                   webhook_url, finalization_started_at, finalization_updated_at,
                   finalization_error, finalizing_progress_percent,
                   created_at, updated_at, expires_at
            FROM uploads WHERE id = ?1
            "#,
        )?;

        let upload = stmt
            .query_row(params![id], |row| {
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
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    AppError::NotFound(format!("Upload {} not found", id))
                }
                _ => AppError::Database(e),
            })?;

        Ok(upload)
    }

    pub fn update_upload_status(&self, id: &str, status: UploadStatus) -> Result<()> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE uploads SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.to_string(), now, id],
        )?;
        Ok(())
    }

    pub fn try_start_finalization(&self, id: &str) -> Result<bool> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().timestamp();
        let affected = conn.execute(
            r#"
            UPDATE uploads
            SET status = 'finalizing',
                finalization_started_at = ?1,
                finalization_updated_at = ?1,
                finalization_error = NULL,
                finalizing_progress_percent = 0,
                updated_at = ?1
            WHERE id = ?2 AND (status = 'pending' OR status = 'failed')
            "#,
            params![now, id],
        )?;
        Ok(affected > 0)
    }

    /// Re-trigger finalization for a stuck upload.
    /// Returns true if the upload was stale and has been reset for retry.
    pub fn restart_stale_finalization(&self, id: &str, stale_threshold_secs: i64) -> Result<bool> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().timestamp();
        let cutoff = now - stale_threshold_secs;
        let affected = conn.execute(
            r#"
            UPDATE uploads
            SET finalization_started_at = ?1,
                finalization_updated_at = ?1,
                finalization_error = NULL,
                finalizing_progress_percent = 0,
                updated_at = ?1
            WHERE id = ?2
              AND status = 'finalizing'
              AND finalization_updated_at < ?3
            "#,
            params![now, id, cutoff],
        )?;
        Ok(affected > 0)
    }

    pub fn update_finalizing_progress(&self, id: &str, progress_percent: i32) -> Result<()> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            r#"
            UPDATE uploads
            SET finalizing_progress_percent = ?1,
                finalization_updated_at = ?2,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![progress_percent.clamp(0, 100), now, id],
        )?;
        Ok(())
    }

    pub fn mark_finalization_complete(&self, id: &str, final_path: &str) -> Result<()> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            r#"
            UPDATE uploads
            SET status = 'complete',
                final_path = ?1,
                finalizing_progress_percent = 100,
                finalization_error = NULL,
                finalization_updated_at = ?2,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![final_path, now, id],
        )?;
        Ok(())
    }

    pub fn mark_finalization_failed(&self, id: &str, error: &str) -> Result<()> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            r#"
            UPDATE uploads
            SET status = 'failed',
                finalization_error = ?1,
                finalization_updated_at = ?2,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![error, now, id],
        )?;
        Ok(())
    }

    pub fn update_upload_final_path(&self, id: &str, final_path: &str) -> Result<()> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "UPDATE uploads SET final_path = ?1, updated_at = ?2 WHERE id = ?3",
            params![final_path, now, id],
        )?;
        Ok(())
    }

    pub fn delete_upload(&self, id: &str) -> Result<()> {
        let conn = self.get_conn()?;
        // Delete parts first
        conn.execute("DELETE FROM upload_parts WHERE upload_id = ?1", params![id])?;
        // Then delete upload
        conn.execute("DELETE FROM uploads WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ============ Part Operations ============

    pub fn create_parts(&self, parts: &[UploadPart]) -> Result<()> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            r#"
            INSERT INTO upload_parts (
                upload_id, part_number, token_hash, status, size, checksum_sha256, uploaded_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )?;

        for part in parts {
            stmt.execute(params![
                part.upload_id,
                part.part_number,
                part.token_hash,
                part.status.to_string(),
                part.size,
                part.checksum_sha256,
                part.uploaded_at,
            ])?;
        }

        Ok(())
    }

    pub fn get_part(&self, upload_id: &str, part_number: i32) -> Result<UploadPart> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT upload_id, part_number, token_hash, status, size, checksum_sha256, uploaded_at
            FROM upload_parts WHERE upload_id = ?1 AND part_number = ?2
            "#,
        )?;

        let part = stmt
            .query_row(params![upload_id, part_number], |row| {
                Ok(UploadPart {
                    upload_id: row.get(0)?,
                    part_number: row.get(1)?,
                    token_hash: row.get(2)?,
                    status: row.get::<_, String>(3)?.into(),
                    size: row.get(4)?,
                    checksum_sha256: row.get(5)?,
                    uploaded_at: row.get(6)?,
                })
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AppError::NotFound(format!(
                    "Part {} not found for upload {}",
                    part_number, upload_id
                )),
                _ => AppError::Database(e),
            })?;

        Ok(part)
    }

    pub fn get_all_parts(&self, upload_id: &str) -> Result<Vec<UploadPart>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT upload_id, part_number, token_hash, status, size, checksum_sha256, uploaded_at
            FROM upload_parts WHERE upload_id = ?1 ORDER BY part_number
            "#,
        )?;

        let parts = stmt
            .query_map(params![upload_id], |row| {
                Ok(UploadPart {
                    upload_id: row.get(0)?,
                    part_number: row.get(1)?,
                    token_hash: row.get(2)?,
                    status: row.get::<_, String>(3)?.into(),
                    size: row.get(4)?,
                    checksum_sha256: row.get(5)?,
                    uploaded_at: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(parts)
    }

    pub fn update_part_status(
        &self,
        upload_id: &str,
        part_number: i32,
        status: PartStatus,
        checksum: Option<&str>,
    ) -> Result<()> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            r#"
            UPDATE upload_parts 
            SET status = ?1, uploaded_at = ?2, checksum_sha256 = ?3
            WHERE upload_id = ?4 AND part_number = ?5
            "#,
            params![status.to_string(), now, checksum, upload_id, part_number],
        )?;
        Ok(())
    }

    pub fn count_uploaded_parts(&self, upload_id: &str) -> Result<i32> {
        let conn = self.get_conn()?;
        let count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM upload_parts WHERE upload_id = ?1 AND status = 'uploaded'",
            params![upload_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    pub fn all_parts_uploaded(&self, upload_id: &str) -> Result<bool> {
        let upload = self.get_upload(upload_id)?;
        let uploaded_count = self.count_uploaded_parts(upload_id)?;
        Ok(uploaded_count == upload.total_parts)
    }
}
