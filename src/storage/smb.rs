//! SMB/NAS Storage Backend using smb-rs (pure Rust SMB client)
//!
//! Parts are stored to fast local disk during upload (keeps HTTP responses fast).
//! Each part is also synced to SMB in the background as it arrives.
//! Finalization checks if SMB file is complete → fast rename. Falls back to streaming if needed.

use async_trait::async_trait;
use bytes::Bytes;
use smb::{
    Client, ClientConfig, CreateDisposition, CreateOptions, Dialect, FileAccessMask,
    FileAttributes, FileCreateArgs, FileDispositionInformation, FileRenameInformation,
    FileStandardInformation, Resource, UncPath, WriteAt,
};
use smb::binrw_util::prelude::SizedWideString;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

use super::{build_response_path, sanitize_target_path, StorageBackend};
use crate::config::Config;
use crate::db::schema::Upload;
use crate::error::{AppError, Result};

/// Max bytes per SMB write_at call. The smb crate does NOT split large writes internally —
/// it sends the entire buffer as one SMB write request. SMB servers typically negotiate
/// MaxWriteSize of 1-4MB. Sending 50MB causes the TCP send to block forever.
/// 1MB is safe for all SMB3 servers.
const SMB_WRITE_CHUNK: usize = 1024 * 1024; // 1MB

pub struct SmbStorage {
    /// Local temp storage for parts (fast SSD)
    parts_path: PathBuf,
    /// SMB connection info
    smb_config: SmbConfig,
    /// SMB client for finalization/management operations
    client: Arc<Mutex<Option<Client>>>,
    /// Dedicated SMB client for background part syncs (separate connection)
    sync_client: Arc<Mutex<Option<Client>>>,
    /// Per-upload write/finalize lock
    upload_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

#[derive(Clone)]
struct SmbConfig {
    unc_path: String,  // \\server\share format
    host: String,      // SMB server hostname/IP
    share: String,     // SMB share name
    port: u16,         // SMB port
    username: String,
    password: String,
    base_path: String, // Path within share (e.g., "Sermons/files")
}

impl SmbStorage {
    /// Create SmbStorage with lazy connection.
    /// Connection is established on first use, not during initialization.
    pub async fn new(config: &Config, temp_storage_path: &str) -> Result<Self> {
        let parts_path = PathBuf::from(temp_storage_path).join("parts");

        let unc_path = if config.smb_port == 445 {
            format!(r"\\{}\{}", config.smb_host, config.smb_share)
        } else {
            format!(r"\\{}:{}\{}", config.smb_host, config.smb_port, config.smb_share)
        };

        let base_path = config.smb_path.trim_matches('/').to_string();

        let smb_config = SmbConfig {
            unc_path,
            host: config.smb_host.clone(),
            share: config.smb_share.clone(),
            port: config.smb_port,
            username: config.smb_user.clone(),
            password: config.smb_pass.clone(),
            base_path,
        };

        tracing::info!("Initializing SmbStorage (lazy connection)...");
        tracing::info!("  SMB: {}", smb_config.unc_path);
        tracing::info!("  User: {}", config.smb_user);
        tracing::info!("  Base path: {}", smb_config.base_path);
        tracing::info!("  Local temp (compat): {}", parts_path.display());

        std::fs::create_dir_all(&parts_path)
            .map_err(|e| AppError::Storage(format!("Failed to create parts directory: {}", e)))?;
        Self::verify_local_write(&parts_path)?;

        tracing::info!("SmbStorage initialized (connection deferred)");

        Ok(Self {
            parts_path,
            smb_config,
            client: Arc::new(Mutex::new(None)),
            sync_client: Arc::new(Mutex::new(None)),
            upload_locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn get_local_part_path(&self, upload_id: &str, part_number: i32) -> PathBuf {
        self.parts_path
            .join(upload_id)
            .join(format!("part_{:06}", part_number))
    }

    async fn get_upload_lock(&self, upload_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.upload_locks.lock().await;
        locks
            .entry(upload_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn remove_upload_lock(&self, upload_id: &str) {
        let mut locks = self.upload_locks.lock().await;
        locks.remove(upload_id);
    }

    async fn get_or_reconnect_client(&self) -> Result<Client> {
        let mut client_guard = self.client.lock().await;

        if client_guard.is_none() {
            tracing::info!("SMB client not connected, reconnecting...");
            let new_client = Self::create_client(&self.smb_config).await?;
            *client_guard = Some(new_client);
        }

        let client = client_guard.take().unwrap();
        drop(client_guard);

        Ok(client)
    }

    async fn return_client(&self, client: Option<Client>) {
        let mut client_guard = self.client.lock().await;
        *client_guard = client;
    }

    async fn create_client(config: &SmbConfig) -> Result<Client> {
        let mut client_config = ClientConfig::default();
        client_config.connection.min_dialect = Some(Dialect::Smb030);
        client_config.connection.max_dialect = Some(Dialect::Smb0311);
        let client = Client::new(client_config);

        tracing::info!("SMB client configured for SMB 3.0+ (min: SMB 3.0, max: SMB 3.1.1)");

        let unc_path = UncPath::from_str(&config.unc_path)
            .map_err(|e| AppError::Storage(format!("Invalid UNC path {}: {}", config.unc_path, e)))?;

        tracing::info!("Attempting SMB connection to: {}", config.unc_path);
        tracing::debug!("  Host: {}:{}", config.host, config.port);
        tracing::debug!("  Share: {}", config.share);
        tracing::debug!("  User: {}", config.username);

        client
            .share_connect(&unc_path, &config.username, config.password.clone())
            .await
            .map_err(|e| {
                let diagnostic = Self::diagnose_connection_error(&e, config);
                AppError::Storage(format!(
                    "Failed to connect to SMB server: {}\n\nDiagnosis: {}\n\nConfiguration:\n  - Host: {}:{}\n  - Share: {}\n  - User: {}\n  - UNC Path: {}",
                    e, diagnostic, config.host, config.port, config.share, config.username, config.unc_path
                ))
            })?;

        tracing::info!("Successfully connected to SMB share: {}", config.unc_path);
        Ok(client)
    }

    fn diagnose_connection_error(err: &dyn std::fmt::Display, config: &SmbConfig) -> String {
        let err_str = err.to_string().to_lowercase();

        if err_str.contains("no route to host") || err_str.contains("connection refused") {
            format!("Network connectivity issue. Cannot reach SMB server at {}", config.unc_path)
        } else if err_str.contains("timeout") || err_str.contains("timed out") {
            "Connection timeout. Server may be unreachable or firewall blocking port".to_string()
        } else if err_str.contains("access denied")
            || err_str.contains("authentication")
            || err_str.contains("login")
        {
            format!("Authentication failed. Check username '{}' and password", config.username)
        } else if err_str.contains("not found") || err_str.contains("does not exist") {
            format!("SMB share '{}' not found on server {}", config.share, config.host)
        } else if err_str.contains("permission") {
            format!("Permission denied. User '{}' may not have access to this share", config.username)
        } else {
            format!("Connection error: {}", err)
        }
    }

    fn verify_local_write(path: &Path) -> Result<()> {
        let test_file = path.join(".write_test");
        let mut file = std::fs::File::create(&test_file).map_err(|e| {
            AppError::Storage(format!("No write permission for parts directory: {}", e))
        })?;
        file.write_all(b"test")
            .map_err(|e| AppError::Storage(format!("Failed write test: {}", e)))?;
        drop(file);
        std::fs::remove_file(&test_file).ok();
        Ok(())
    }

    async fn ensure_smb_dirs_for_path(
        client: &Client,
        config: &SmbConfig,
        smb_path: &str,
    ) -> Result<()> {
        let normalized_path = smb_path.replace('\\', "/");
        let dir_path = match normalized_path.rsplit_once('/') {
            Some((dir, _)) => dir,
            None => "",
        };

        if dir_path.is_empty() {
            return Ok(());
        }

        let parts: Vec<&str> = dir_path.split('/').filter(|s| !s.is_empty()).collect();
        let unc_path = UncPath::from_str(&config.unc_path)
            .map_err(|e| AppError::Storage(format!("Invalid UNC path: {}", e)))?;

        let mut cumulative_path = String::new();

        for part in parts {
            if cumulative_path.is_empty() {
                cumulative_path = part.to_string();
            } else {
                cumulative_path = format!("{}/{}", cumulative_path, part);
            }

            let current_path = unc_path.clone().with_path(&cumulative_path);

            let open_args = FileCreateArgs::make_open_existing(
                FileAccessMask::new().with_generic_read(true),
            );

            match client.create_file(&current_path, &open_args).await {
                Ok(Resource::Directory(dir)) => {
                    dir.close().await.ok();
                }
                Ok(Resource::File(_)) => {
                    return Err(AppError::Storage(format!(
                        "Path exists but is a file, not directory: {}",
                        cumulative_path
                    )));
                }
                Err(_) => {
                    let create_args = FileCreateArgs {
                        desired_access: FileAccessMask::new().with_generic_write(true),
                        attributes: FileAttributes::new().with_directory(true),
                        disposition: CreateDisposition::Create,
                        options: CreateOptions::new().with_directory_file(true),
                    };

                    match client.create_file(&current_path, &create_args).await {
                        Ok(Resource::Directory(dir)) => {
                            dir.close().await.ok();
                        }
                        Ok(_) => {
                            return Err(AppError::Storage(format!(
                                "Failed to create directory {}: unexpected resource type",
                                cumulative_path
                            )));
                        }
                        Err(e) => {
                            let err_str = e.to_string().to_lowercase();
                            if !(err_str.contains("exists") || err_str.contains("object name collision")) {
                                return Err(AppError::Storage(format!(
                                    "Failed to create SMB directory {}: {}",
                                    cumulative_path, e
                                )));
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn get_smb_file_path(
        &self,
        upload_id: &str,
        filename: &str,
        target_path: Option<&str>,
    ) -> String {
        let safe_filename = Path::new(filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed");

        let final_filename = format!("{}_{}", upload_id, safe_filename);
        let base_path = self.smb_config.base_path.trim_matches('/');

        match target_path {
            Some(path) => {
                let clean_path = sanitize_target_path(path);
                if clean_path.is_empty() {
                    if base_path.is_empty() {
                        final_filename
                    } else {
                        format!("{}/{}", base_path, final_filename)
                    }
                } else if base_path.is_empty() {
                    format!("{}/{}", clean_path, final_filename)
                } else {
                    format!("{}/{}/{}", base_path, clean_path, final_filename)
                }
            }
            None => {
                if base_path.is_empty() {
                    format!("files/{}", final_filename)
                } else {
                    format!("{}/files/{}", base_path, final_filename)
                }
            }
        }
    }

    fn get_smb_partial_path(
        &self,
        upload_id: &str,
        filename: &str,
        target_path: Option<&str>,
    ) -> String {
        format!(
            "{}.partial",
            self.get_smb_file_path(upload_id, filename, target_path)
        )
    }

    fn normalize_relative_path(config: &SmbConfig, path: &str) -> String {
        let clean = path.trim_matches(|c| c == '\\' || c == '/');
        let base = config.base_path.trim_matches('/');

        if clean.is_empty() {
            return base.to_string();
        }

        if base.is_empty() {
            return clean.to_string();
        }

        let base_prefix = format!("{}/", base);
        if clean == base || clean.starts_with(&base_prefix) {
            return clean.to_string();
        }

        if base.ends_with("files") {
            if clean == "files" {
                return base.to_string();
            }
            if let Some(suffix) = clean.strip_prefix("files/") {
                if suffix.is_empty() {
                    return base.to_string();
                }
                return format!("{}/{}", base, suffix);
            }
        }

        format!("{}/{}", base, clean)
    }

    fn build_unc_path_from_relative(config: &SmbConfig, path: &str) -> Result<UncPath> {
        let base_unc = UncPath::from_str(&config.unc_path)
            .map_err(|e| AppError::Storage(format!("Invalid UNC path {}: {}", config.unc_path, e)))?;
        let normalized = Self::normalize_relative_path(config, path);
        Ok(base_unc.with_path(&normalized))
    }

    async fn delete_smb_resource(client: &Client, unc_path: &UncPath) -> Result<()> {
        let delete_args = FileCreateArgs::make_open_existing(
            FileAccessMask::new()
                .with_generic_read(true)
                .with_delete(true),
        );

        let resource = match client.create_file(unc_path, &delete_args).await {
            Ok(resource) => resource,
            Err(e) => {
                let err_str = e.to_string().to_lowercase();
                if err_str.contains("not found") || err_str.contains("does not exist") {
                    return Ok(());
                }
                return Err(AppError::Storage(format!(
                    "Failed to open SMB path for delete: {}",
                    e
                )));
            }
        };

        match resource {
            Resource::File(file) => {
                file.set_info(FileDispositionInformation {
                    delete_pending: true.into(),
                })
                .await
                .map_err(|e| AppError::Storage(format!("Failed to delete SMB file: {}", e)))?;
                file.close().await.ok();
            }
            Resource::Directory(dir) => {
                dir.set_info(FileDispositionInformation {
                    delete_pending: true.into(),
                })
                .await
                .map_err(|e| AppError::Storage(format!("Failed to delete SMB directory: {}", e)))?;
                dir.close().await.ok();
            }
            _ => {
                return Err(AppError::Storage(
                    "Unexpected SMB resource type for delete".to_string(),
                ));
            }
        }

        Ok(())
    }

    async fn open_or_create_for_write(
        &self,
        client: &Client,
        smb_path_str: &str,
    ) -> Result<smb::File> {
        Self::ensure_smb_dirs_for_path(client, &self.smb_config, smb_path_str).await?;
        let unc_path = Self::build_unc_path_from_relative(&self.smb_config, smb_path_str)?;

        let create_args = FileCreateArgs {
            disposition: CreateDisposition::OpenIf,
            attributes: FileAttributes::default(),
            options: CreateOptions::new().with_non_directory_file(true),
            desired_access: FileAccessMask::new()
                .with_generic_read(true)
                .with_generic_write(true)
                .with_delete(true),
        };

        match client.create_file(&unc_path, &create_args).await {
            Ok(Resource::File(file)) => Ok(file),
            Ok(_) => Err(AppError::Storage(format!(
                "Expected file resource for {}",
                smb_path_str
            ))),
            Err(e) => Err(AppError::Storage(format!(
                "Failed to open/create SMB file {}: {}",
                smb_path_str, e
            ))),
        }
    }

    async fn open_existing_file(
        &self,
        client: &Client,
        smb_path_str: &str,
        with_delete: bool,
    ) -> Result<smb::File> {
        let unc_path = Self::build_unc_path_from_relative(&self.smb_config, smb_path_str)?;
        let mut access = FileAccessMask::new()
            .with_generic_read(true)
            .with_generic_write(true);
        if with_delete {
            access = access.with_delete(true);
        }

        let open_args = FileCreateArgs::make_open_existing(access);
        match client.create_file(&unc_path, &open_args).await {
            Ok(Resource::File(file)) => Ok(file),
            Ok(_) => Err(AppError::Storage(format!(
                "Expected file resource for {}",
                smb_path_str
            ))),
            Err(e) => Err(AppError::Storage(format!(
                "Failed to open SMB file {}: {}",
                smb_path_str, e
            ))),
        }
    }

    /// Get the size of an SMB file. Returns 0 if file doesn't exist.
    async fn get_smb_file_size(&self, client: &Client, smb_path_str: &str) -> Result<u64> {
        let unc_path = Self::build_unc_path_from_relative(&self.smb_config, smb_path_str)?;
        let open_args = FileCreateArgs::make_open_existing(
            FileAccessMask::new().with_generic_read(true),
        );

        match client.create_file(&unc_path, &open_args).await {
            Ok(Resource::File(file)) => {
                let info: FileStandardInformation = file
                    .query_info()
                    .await
                    .map_err(|e| AppError::Storage(format!("Failed to query SMB file info: {}", e)))?;
                let size = info.end_of_file;
                file.close().await.ok();
                Ok(size)
            }
            Ok(_) => Ok(0),
            Err(_) => Ok(0), // File doesn't exist yet
        }
    }
}

/// Fire-and-forget: sync one part to the SMB partial file at the correct offset.
/// Uses its own dedicated client connection (separate from finalization client).
/// Entire operation has a 120s timeout to prevent infinite hangs.
async fn background_sync_part(
    sync_client: Arc<Mutex<Option<Client>>>,
    smb_config: &SmbConfig,
    partial_path: &str,
    offset: u64,
    data: Bytes,
) -> std::result::Result<(), String> {
    const SYNC_TIMEOUT: Duration = Duration::from_secs(120);

    match timeout(SYNC_TIMEOUT, background_sync_part_inner(
        sync_client.clone(),
        smb_config,
        partial_path,
        offset,
        data,
    ))
    .await
    {
        Ok(result) => result,
        Err(_) => {
            // Timeout — drop the client (likely dead connection)
            sync_client.lock().await.take();
            Err(format!("Background sync timed out after {}s", SYNC_TIMEOUT.as_secs()))
        }
    }
}

async fn background_sync_part_inner(
    sync_client: Arc<Mutex<Option<Client>>>,
    smb_config: &SmbConfig,
    partial_path: &str,
    offset: u64,
    data: Bytes,
) -> std::result::Result<(), String> {
    // Get or reconnect the dedicated sync client
    let client = {
        let mut guard = sync_client.lock().await;
        if guard.is_none() {
            let new_client = SmbStorage::create_client(smb_config)
                .await
                .map_err(|e| format!("Sync client connect failed: {}", e))?;
            *guard = Some(new_client);
        }
        guard.take().unwrap()
    };

    let write_result = async {
        // Ensure dirs + open/create the partial file
        SmbStorage::ensure_smb_dirs_for_path(&client, smb_config, partial_path)
            .await
            .map_err(|e| format!("ensure dirs: {}", e))?;

        let unc_path = SmbStorage::build_unc_path_from_relative(smb_config, partial_path)
            .map_err(|e| format!("build UNC: {}", e))?;

        let create_args = FileCreateArgs {
            disposition: CreateDisposition::OpenIf,
            attributes: FileAttributes::default(),
            options: CreateOptions::new().with_non_directory_file(true),
            desired_access: FileAccessMask::new()
                .with_generic_read(true)
                .with_generic_write(true),
        };

        let file = match client.create_file(&unc_path, &create_args).await {
            Ok(Resource::File(f)) => f,
            Ok(_) => return Err("Expected file resource".to_string()),
            Err(e) => return Err(format!("open partial file: {}", e)),
        };

        // Write at the correct offset, chunked to SMB_WRITE_CHUNK (1MB)
        let mut written = 0usize;
        while written < data.len() {
            let end = (written + SMB_WRITE_CHUNK).min(data.len());
            let bytes = file
                .write_at(&data[written..end], offset + written as u64)
                .await
                .map_err(|e| format!("write_at: {}", e))?;
            if bytes == 0 {
                return Err("write_at returned 0 bytes".to_string());
            }
            written += bytes;
        }

        file.close().await.ok();
        Ok::<(), String>(())
    }
    .await;

    // Return client to pool (even on error, try to keep the connection)
    match &write_result {
        Ok(()) => {
            sync_client.lock().await.replace(client);
        }
        Err(_) => {
            // Drop the client on error (force reconnect next time)
        }
    }

    write_result
}

#[async_trait]
impl StorageBackend for SmbStorage {
    /// Store part to local disk (fast), then fire-and-forget background sync to SMB.
    async fn store_part(
        &self,
        upload: &Upload,
        part_number: i32,
        data: Bytes,
    ) -> Result<String> {
        let upload_id = &upload.id;
        let part_path = self.get_local_part_path(upload_id, part_number);
        let data_len = data.len();

        // Phase 1: Write to local disk (fast — this is what the HTTP handler waits on)
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
            "Stored part {} for upload {} locally ({} bytes)",
            part_number, upload_id, data_len
        );

        // Phase 2: Fire-and-forget background sync to SMB
        let partial_path = self.get_smb_partial_path(
            upload_id,
            &upload.filename,
            upload.target_path.as_deref(),
        );
        let offset = (part_number as u64)
            .checked_mul(upload.chunk_size as u64)
            .unwrap_or(0);
        let sync_client = self.sync_client.clone();
        let smb_config = self.smb_config.clone();
        let upload_id_owned = upload_id.to_string();

        tokio::spawn(async move {
            if let Err(e) = background_sync_part(
                sync_client,
                &smb_config,
                &partial_path,
                offset,
                data,
            )
            .await
            {
                tracing::warn!(
                    "Background SMB sync failed for upload {} part {} (will retry during finalization): {}",
                    upload_id_owned, part_number, e
                );
            } else {
                tracing::debug!(
                    "Background SMB sync complete for upload {} part {} ({} bytes)",
                    upload_id_owned, part_number, data_len
                );
            }
        });

        Ok(part_path.to_string_lossy().to_string())
    }

    async fn read_part(&self, upload_id: &str, part_number: i32) -> Result<Bytes> {
        let part_path = self.get_local_part_path(upload_id, part_number);
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
        // This is called by the default finalize_upload, but we override finalize_upload,
        // so this is only here as a fallback.
        let _ = (upload_id, filename, total_parts, target_path);
        Err(AppError::Storage(
            "SMB backend uses finalize_upload directly; assemble_parts not supported".to_string(),
        ))
    }

    /// Verify all local parts exist and have correct total size.
    async fn verify_upload_ready(&self, upload: &Upload) -> Result<()> {
        let upload_id = &upload.id;
        let mut total_bytes: u64 = 0;

        for part_num in 0..upload.total_parts {
            let part_path = self.get_local_part_path(upload_id, part_num);
            let metadata = fs::metadata(&part_path).await.map_err(|e| {
                AppError::Storage(format!(
                    "Finalization verification failed: part {} missing for upload {}: {}",
                    part_num, upload_id, e
                ))
            })?;
            total_bytes += metadata.len();
        }

        let expected = upload.total_size as u64;
        if total_bytes != expected {
            return Err(AppError::Storage(format!(
                "Finalization verification failed: total parts size mismatch for upload {} (expected {}, got {})",
                upload_id, expected, total_bytes
            )));
        }

        Ok(())
    }

    /// Finalize upload to SMB.
    /// Fast path: background syncs already wrote everything → just verify size and rename.
    /// Slow path: stream any missing data from local parts, then rename.
    /// Every SMB operation has a timeout to prevent infinite hangs.
    async fn finalize_upload(&self, upload: &Upload) -> Result<String> {
        // Timeouts: 60s per part write, 30s for metadata ops, 10 min total per attempt
        const PER_PART_TIMEOUT: Duration = Duration::from_secs(120);
        const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
        const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

        let upload_id = &upload.id;
        let lock = self.get_upload_lock(upload_id).await;
        let _guard = lock.lock().await;

        let partial_path =
            self.get_smb_partial_path(upload_id, &upload.filename, upload.target_path.as_deref());
        let final_path =
            self.get_smb_file_path(upload_id, &upload.filename, upload.target_path.as_deref());
        let expected_size = upload.total_size as u64;

        let mut last_error: Option<AppError> = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tracing::warn!(
                    "Retrying SMB finalization for upload {} attempt {}/3",
                    upload_id,
                    attempt + 1
                );
                self.return_client(None).await;
                // Back off before retry
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt as u32))).await;
            }

            let client = match timeout(CONNECT_TIMEOUT, self.get_or_reconnect_client()).await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    last_error = Some(AppError::Storage(format!(
                        "SMB reconnect failed during finalization: {}",
                        e
                    )));
                    continue;
                }
                Err(_) => {
                    last_error = Some(AppError::Storage(
                        "SMB connection timed out (15s)".to_string(),
                    ));
                    continue;
                }
            };

            let stream_result = async {
                // Check if background syncs already completed the file
                let current_size = match timeout(
                    METADATA_TIMEOUT,
                    self.get_smb_file_size(&client, &partial_path),
                )
                .await
                {
                    Ok(Ok(size)) => size,
                    _ => 0, // Timeout or error → assume nothing synced
                };

                if current_size >= expected_size {
                    tracing::info!(
                        "Fast path: background syncs completed for upload {} ({} bytes already on SMB)",
                        upload_id,
                        current_size
                    );
                } else {
                    // Slow path: stream parts that haven't been synced yet
                    let chunk_size = upload.chunk_size as u64;
                    let first_missing_part = if chunk_size > 0 {
                        (current_size / chunk_size) as i32
                    } else {
                        0
                    };

                    tracing::info!(
                        "Streaming to SMB: {} of {} bytes done for upload {}, resuming from part {}/{}",
                        current_size,
                        expected_size,
                        upload_id,
                        first_missing_part,
                        upload.total_parts
                    );

                    let file = timeout(
                        METADATA_TIMEOUT,
                        self.open_or_create_for_write(&client, &partial_path),
                    )
                    .await
                    .map_err(|_| AppError::Storage("Timeout opening SMB partial file".to_string()))?
                    .map_err(|e| {
                        AppError::Storage(format!("Failed to open SMB partial file: {}", e))
                    })?;

                    let mut offset = first_missing_part as u64 * chunk_size;
                    for part_num in first_missing_part..upload.total_parts {
                        let part_path = self.get_local_part_path(upload_id, part_num);
                        let part_data = fs::read(&part_path).await.map_err(|e| {
                            AppError::Storage(format!(
                                "Failed to read part {} during finalization: {}",
                                part_num, e
                            ))
                        })?;

                        // Wrap the entire part write in a timeout.
                        // CRITICAL: chunk each write_at to SMB_WRITE_CHUNK (1MB).
                        // The smb crate sends the entire buffer as one SMB request —
                        // 50MB requests hang forever on the TCP send.
                        let part_len = part_data.len();
                        let write_result = timeout(PER_PART_TIMEOUT, async {
                            let mut written = 0usize;
                            while written < part_data.len() {
                                let end = (written + SMB_WRITE_CHUNK).min(part_data.len());
                                let bytes = file
                                    .write_at(
                                        &part_data[written..end],
                                        offset + written as u64,
                                    )
                                    .await
                                    .map_err(|e| {
                                        AppError::Storage(format!(
                                            "SMB write_at failed for part {}: {}",
                                            part_num, e
                                        ))
                                    })?;

                                if bytes == 0 {
                                    return Err(AppError::Storage(
                                        "SMB write returned 0 bytes".to_string(),
                                    ));
                                }
                                written += bytes;
                            }
                            Ok::<(), AppError>(())
                        })
                        .await;

                        match write_result {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => return Err(e),
                            Err(_) => {
                                return Err(AppError::Storage(format!(
                                    "SMB write timed out for part {} ({}s limit)",
                                    part_num,
                                    PER_PART_TIMEOUT.as_secs()
                                )));
                            }
                        }

                        offset += part_len as u64;
                        tracing::info!(
                            "Streamed part {}/{} ({} bytes) to SMB for upload {}",
                            part_num + 1,
                            upload.total_parts,
                            part_len,
                            upload_id
                        );
                    }

                    if let Err(e) = file.close().await {
                        tracing::warn!("Failed to close SMB partial file: {}", e);
                    }
                }

                Ok::<(), AppError>(())
            }
            .await;

            match stream_result {
                Ok(()) => {
                    // Rename partial → final (with timeout)
                    let rename_result = timeout(METADATA_TIMEOUT, async {
                        Self::ensure_smb_dirs_for_path(&client, &self.smb_config, &final_path)
                            .await?;

                        let file =
                            self.open_existing_file(&client, &partial_path, true).await?;
                        file.set_info(FileRenameInformation {
                            replace_if_exists: true.into(),
                            root_directory: 0,
                            file_name: SizedWideString::from(final_path.clone()),
                        })
                        .await
                        .map_err(|e| {
                            AppError::Storage(format!(
                                "Failed to rename SMB partial file: {}",
                                e
                            ))
                        })?;

                        if let Err(e) = file.close().await {
                            tracing::warn!("Failed to close SMB file after rename: {}", e);
                        }

                        Ok::<(), AppError>(())
                    })
                    .await;

                    self.return_client(Some(client)).await;

                    match rename_result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => return Err(e),
                        Err(_) => {
                            return Err(AppError::Storage(
                                "SMB rename timed out (30s)".to_string(),
                            ));
                        }
                    }

                    // Clean up local parts
                    let local_parts_dir = self.parts_path.join(upload_id);
                    if local_parts_dir.exists() {
                        if let Err(e) = fs::remove_dir_all(&local_parts_dir).await {
                            tracing::warn!(
                                "Failed to clean up local parts for upload {}: {}",
                                upload_id,
                                e
                            );
                        }
                    }

                    self.remove_upload_lock(upload_id).await;

                    let response_path = build_response_path(
                        upload_id,
                        &upload.filename,
                        upload.target_path.as_deref(),
                    );
                    tracing::info!(
                        "Finalized upload {} to SMB path {}",
                        upload_id,
                        final_path
                    );
                    return Ok(response_path);
                }
                Err(e) => {
                    tracing::error!(
                        "SMB finalization attempt {} failed for upload {}: {}",
                        attempt + 1,
                        upload_id,
                        e
                    );
                    self.return_client(None).await; // Drop broken client
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            AppError::Storage("SMB finalization failed after retries".to_string())
        }))
    }

    async fn cleanup_incomplete_upload(&self, upload: &Upload) -> Result<()> {
        // Clean up any SMB partial file (may exist from previous or current attempt)
        let partial_path =
            self.get_smb_partial_path(&upload.id, &upload.filename, upload.target_path.as_deref());

        if let Ok(client) = self.get_or_reconnect_client().await {
            if let Ok(unc_path) =
                Self::build_unc_path_from_relative(&self.smb_config, &partial_path)
            {
                if let Err(e) = Self::delete_smb_resource(&client, &unc_path).await {
                    tracing::warn!(
                        "Failed to delete SMB partial file for upload {}: {}",
                        upload.id,
                        e
                    );
                }
            }
            self.return_client(Some(client)).await;
        }

        // Clean up local parts
        let local_parts = self.parts_path.join(&upload.id);
        if local_parts.exists() {
            if let Err(e) = fs::remove_dir_all(&local_parts).await {
                tracing::warn!(
                    "Failed to remove local parts for upload {}: {}",
                    upload.id,
                    e
                );
            }
        }

        self.remove_upload_lock(&upload.id).await;
        Ok(())
    }

    async fn delete_parts(&self, upload_id: &str) -> Result<()> {
        let parts_dir = self.parts_path.join(upload_id);
        if parts_dir.exists() {
            fs::remove_dir_all(&parts_dir).await.map_err(|e| {
                AppError::Storage(format!("Failed to delete parts directory: {}", e))
            })?;
        }
        self.remove_upload_lock(upload_id).await;
        Ok(())
    }

    async fn delete_file(&self, path: &str) -> Result<()> {
        let unc_path = match UncPath::from_str(path) {
            Ok(unc_path) => unc_path,
            Err(_) => Self::build_unc_path_from_relative(&self.smb_config, path)?,
        };

        let client = self.get_or_reconnect_client().await?;
        let result = Self::delete_smb_resource(&client, &unc_path).await;
        self.return_client(Some(client)).await;

        if result.is_ok() {
            tracing::debug!("Deleted SMB path: {}", path);
        }
        result
    }

    fn backend_type(&self) -> &'static str {
        "smb"
    }

    async fn health_check(&self) -> (bool, Option<String>) {
        let has_client = {
            let guard = self.client.lock().await;
            guard.is_some()
        };

        if has_client {
            return (true, Some("SMB connected".to_string()));
        }

        match Self::create_client(&self.smb_config).await {
            Ok(client) => {
                self.return_client(Some(client)).await;
                (true, Some("SMB connection established".to_string()))
            }
            Err(e) => {
                let msg = format!(
                    "SMB unavailable: {} ({}:{})",
                    e, self.smb_config.host, self.smb_config.port
                );
                (false, Some(msg))
            }
        }
    }
}
