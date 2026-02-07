//! SMB/NAS Storage Backend using smb-rs (pure Rust SMB client)
//!
//! Chunks are written directly to an SMB partial file during part uploads.
//! Finalization verifies file size and atomically renames `.partial` to final path.

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
use tokio::sync::Mutex;

use super::StorageBackend;
use crate::config::Config;
use crate::db::schema::Upload;
use crate::error::{AppError, Result};

pub struct SmbStorage {
    /// Local temp storage (kept for compatibility/cleanup of old uploads)
    parts_path: PathBuf,
    /// SMB connection info
    smb_config: SmbConfig,
    /// SMB client connection pool (single reusable client)
    client: Arc<Mutex<Option<Client>>>,
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
            upload_locks: Arc::new(Mutex::new(HashMap::new())),
        })
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

    fn verify_local_write(path: &PathBuf) -> Result<()> {
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
                        ..Default::default()
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

    fn sanitize_target_path(path: &str) -> String {
        path.trim_matches('/')
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '/' || *c == '.' || *c == '-' || *c == '_')
            .collect()
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
                let clean_path = Self::sanitize_target_path(path);
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

    fn get_smb_response_path(
        upload_id: &str,
        filename: &str,
        target_path: Option<&str>,
    ) -> String {
        let safe_filename = Path::new(filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed");

        let final_filename = format!("{}_{}", upload_id, safe_filename);

        match target_path {
            Some(path) => {
                let clean_path = Self::sanitize_target_path(path);
                if clean_path.is_empty() {
                    final_filename
                } else {
                    format!("{}/{}", clean_path, final_filename)
                }
            }
            None => format!("files/{}", final_filename),
        }
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
}

#[async_trait]
impl StorageBackend for SmbStorage {
    async fn store_part(
        &self,
        upload: &Upload,
        part_number: i32,
        data: Bytes,
    ) -> Result<String> {
        let upload_id = &upload.id;
        let lock = self.get_upload_lock(upload_id).await;
        let _guard = lock.lock().await;

        let partial_path =
            self.get_smb_partial_path(upload_id, &upload.filename, upload.target_path.as_deref());
        let offset = (part_number as u64)
            .checked_mul(upload.chunk_size as u64)
            .ok_or_else(|| AppError::Storage("Part offset overflow".to_string()))?;

        let mut last_error: Option<AppError> = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tracing::warn!(
                    "Retrying SMB part write for upload {} part {} attempt {}/3",
                    upload_id,
                    part_number,
                    attempt + 1
                );
                self.return_client(None).await;
            }

            let client = match self.get_or_reconnect_client().await {
                Ok(c) => c,
                Err(e) => {
                    last_error = Some(AppError::Storage(format!(
                        "SMB reconnect failed for part write: {}",
                        e
                    )));
                    continue;
                }
            };

            let write_result = async {
                let file = self.open_or_create_for_write(&client, &partial_path).await?;

                let mut written = 0usize;
                while written < data.len() {
                    let bytes = file
                        .write_at(&data[written..], offset + written as u64)
                        .await
                        .map_err(|e| AppError::Storage(format!("Failed to write SMB part: {}", e)))?;

                    if bytes == 0 {
                        return Err(AppError::Storage(
                            "SMB write returned 0 bytes".to_string(),
                        ));
                    }
                    written += bytes;
                }

                if let Err(e) = file.close().await {
                    tracing::warn!("Failed to close SMB file after part write: {}", e);
                }

                Ok::<(), AppError>(())
            }
            .await;

            match write_result {
                Ok(()) => {
                    self.return_client(Some(client)).await;
                    tracing::debug!(
                        "Stored part {} for upload {} directly to SMB partial file ({} bytes)",
                        part_number,
                        upload_id,
                        data.len()
                    );
                    return Ok(partial_path);
                }
                Err(e) => {
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            AppError::Storage("SMB part write failed after retries".to_string())
        }))
    }

    async fn read_part(&self, upload_id: &str, part_number: i32) -> Result<Bytes> {
        let part_path = self
            .parts_path
            .join(upload_id)
            .join(format!("part_{:06}", part_number));
        let data = fs::read(&part_path).await.map_err(|e| {
            AppError::Storage(format!(
                "Failed to read local compatibility part {} for upload {}: {}",
                part_number, upload_id, e
            ))
        })?;
        Ok(Bytes::from(data))
    }

    async fn assemble_parts(
        &self,
        _upload_id: &str,
        _filename: &str,
        _total_parts: i32,
        _target_path: Option<&str>,
    ) -> Result<String> {
        Err(AppError::Storage(
            "SMB direct mode does not use assemble_parts; call finalize_upload instead".to_string(),
        ))
    }

    async fn verify_upload_ready(&self, upload: &Upload) -> Result<()> {
        let upload_id = &upload.id;
        let lock = self.get_upload_lock(upload_id).await;
        let _guard = lock.lock().await;

        let partial_path =
            self.get_smb_partial_path(upload_id, &upload.filename, upload.target_path.as_deref());

        let client = self.get_or_reconnect_client().await?;
        let verify_result = async {
            let file = self.open_existing_file(&client, &partial_path, false).await?;
            let info = file
                .query_info::<FileStandardInformation>()
                .await
                .map_err(|e| AppError::Storage(format!("Failed to query SMB file info: {}", e)))?;

            if let Err(e) = file.close().await {
                tracing::warn!("Failed to close SMB file after verify: {}", e);
            }

            let expected = upload.total_size as u64;
            if info.end_of_file != expected {
                return Err(AppError::Storage(format!(
                    "Finalization verification failed: partial file size mismatch for upload {} (expected {}, got {})",
                    upload_id, expected, info.end_of_file
                )));
            }

            Ok::<(), AppError>(())
        }
        .await;

        self.return_client(Some(client)).await;
        verify_result
    }

    async fn finalize_upload(&self, upload: &Upload) -> Result<String> {
        let upload_id = &upload.id;
        let lock = self.get_upload_lock(upload_id).await;
        let _guard = lock.lock().await;

        let partial_path =
            self.get_smb_partial_path(upload_id, &upload.filename, upload.target_path.as_deref());
        let final_path = self.get_smb_file_path(upload_id, &upload.filename, upload.target_path.as_deref());

        let client = self.get_or_reconnect_client().await?;
        let finalize_result = async {
            // Ensure destination directory exists even when no part write happened in this process.
            Self::ensure_smb_dirs_for_path(&client, &self.smb_config, &final_path).await?;

            let file = self.open_existing_file(&client, &partial_path, true).await?;
            file.set_info(FileRenameInformation {
                replace_if_exists: true.into(),
                root_directory: 0,
                file_name: SizedWideString::from(final_path.clone()),
            })
            .await
            .map_err(|e| AppError::Storage(format!("Failed to rename SMB partial file: {}", e)))?;

            if let Err(e) = file.close().await {
                tracing::warn!("Failed to close SMB file after rename: {}", e);
            }

            Ok::<(), AppError>(())
        }
        .await;

        self.return_client(Some(client)).await;
        finalize_result?;

        self.remove_upload_lock(upload_id).await;

        let response_path =
            Self::get_smb_response_path(upload_id, &upload.filename, upload.target_path.as_deref());
        tracing::info!("Finalized upload {} to SMB path {}", upload_id, final_path);
        Ok(response_path)
    }

    async fn cleanup_incomplete_upload(&self, upload: &Upload) -> Result<()> {
        let partial_path =
            self.get_smb_partial_path(&upload.id, &upload.filename, upload.target_path.as_deref());

        let client = self.get_or_reconnect_client().await?;
        let unc_path = Self::build_unc_path_from_relative(&self.smb_config, &partial_path)?;
        let delete_result = Self::delete_smb_resource(&client, &unc_path).await;
        self.return_client(Some(client)).await;

        if let Err(e) = delete_result {
            tracing::warn!(
                "Failed to delete SMB partial file for upload {}: {}",
                upload.id,
                e
            );
        }

        // Backward compatibility cleanup for any local parts created by previous versions.
        let local_parts = self.parts_path.join(&upload.id);
        if local_parts.exists() {
            if let Err(e) = fs::remove_dir_all(&local_parts).await {
                tracing::warn!(
                    "Failed to remove local compatibility parts for upload {}: {}",
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
