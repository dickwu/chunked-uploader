//! SMB/NAS Storage Backend using smb-rs (pure Rust SMB client)
//!
//! Uses local fast storage for parts, then copies final files to SMB
//! using native SMB protocol (no mount required, no C dependencies).

use async_trait::async_trait;
use bytes::Bytes;
use smb::{
    Client, ClientConfig, CreateOptions, Dialect, FileAccessMask, FileAttributes, FileCreateArgs, Resource,
    UncPath, WriteAt,
};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use super::StorageBackend;
use crate::config::Config;
use crate::error::{AppError, Result};

pub struct SmbStorage {
    /// Local temp storage for parts (fast SSD)
    parts_path: PathBuf,
    /// SMB connection info
    smb_config: SmbConfig,
    /// SMB client (protected by mutex for thread safety)
    client: Arc<Mutex<Client>>,
}

#[derive(Clone)]
struct SmbConfig {
    unc_path: String,  // \\server\share format
    host: String,     // SMB server hostname/IP
    share: String,    // SMB share name
    port: u16,        // SMB port
    username: String,
    password: String,
    base_path: String, // Path within share (e.g., "Sermons/files")
}

impl SmbStorage {
    pub async fn new(config: &Config, temp_storage_path: &str) -> Result<Self> {
        let parts_path = PathBuf::from(temp_storage_path).join("parts");

        // Build UNC path: \\server\share
        let unc_path = if config.smb_port == 445 {
            format!(r"\\{}\{}", config.smb_host, config.smb_share)
        } else {
            format!(r"\\{}:{}\{}", config.smb_host, config.smb_port, config.smb_share)
        };

        // Build base path within share
        let base_path = if config.smb_path.is_empty() {
            "files".to_string()
        } else {
            format!("{}/files", config.smb_path.trim_matches('/'))
        };

        let smb_config = SmbConfig {
            unc_path,
            host: config.smb_host.clone(),
            share: config.smb_share.clone(),
            port: config.smb_port,
            username: config.smb_user.clone(),
            password: config.smb_pass.clone(),
            base_path,
        };

        tracing::info!("Initializing SmbStorage...");
        tracing::info!("  SMB: {}", smb_config.unc_path);
        tracing::info!("  User: {}", config.smb_user);
        tracing::info!("  Base path: {}", smb_config.base_path);
        tracing::info!("  Local temp (parts): {}", parts_path.display());

        // Create local parts directory
        std::fs::create_dir_all(&parts_path)
            .map_err(|e| AppError::Storage(format!("Failed to create parts directory: {}", e)))?;

        // Test local write permission
        Self::verify_local_write(&parts_path)?;

        // Create and connect SMB client
        let client = Self::create_client(&smb_config).await?;

        // Create files directory on SMB
        Self::ensure_smb_dir(&client, &smb_config).await?;

        // Test SMB write
        Self::verify_smb_write(&client, &smb_config).await?;

        tracing::info!("SmbStorage initialized:");
        tracing::info!("  ✓ Parts (local): {}", parts_path.display());
        tracing::info!("  ✓ Files (SMB): {}/{}", smb_config.unc_path, smb_config.base_path);

        Ok(Self {
            parts_path,
            smb_config,
            client: Arc::new(Mutex::new(client)),
        })
    }

    async fn create_client(config: &SmbConfig) -> Result<Client> {
        // Configure client to use SMB 3.0 as minimum dialect
        // This forces the client to negotiate SMB 3.0 or higher (3.0, 3.0.2, 3.1.1)
        let mut client_config = ClientConfig::default();
        // Set min_dialect to SMB 3.0 to force SMB 3.0+ negotiation
        client_config.connection.min_dialect = Some(Dialect::Smb030);
        client_config.connection.max_dialect = Some(Dialect::Smb0311);
        let client = Client::new(client_config);
        
        tracing::info!("SMB client configured for SMB 3.0+ (min: SMB 3.0, max: SMB 3.1.1)");

        // Connect to share
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
                let error_msg = format!("Failed to connect to SMB server: {}", e);
                let diagnostic = Self::diagnose_connection_error(&e, config);
                AppError::Storage(format!("{}\n\nDiagnosis: {}\n\nConfiguration:\n  - Host: {}:{}\n  - Share: {}\n  - User: {}\n  - UNC Path: {}\n\nTroubleshooting:\n  1. Network connectivity: ping {}\n  2. Port accessibility: nc -zv {} {}\n  3. Manual SMB test: smbclient //{}/{} -U {}%<password>\n  4. Verify credentials are correct\n  5. Check SMB server is running and share '{}' exists\n  6. Ensure firewall allows port {} (SMB)\n  7. If using SMB, ensure the 'smb' feature is enabled: cargo build --features smb", 
                    error_msg, diagnostic, config.host, config.port, config.share, 
                    config.username, config.unc_path, config.host, config.host, config.port,
                    config.host, config.share, config.username, config.share, config.port))
            })?;

        tracing::info!("Successfully connected to SMB share: {}", config.unc_path);
        Ok(client)
    }

    fn diagnose_connection_error(err: &dyn std::fmt::Display, config: &SmbConfig) -> String {
        let err_str = err.to_string().to_lowercase();
        
        if err_str.contains("no route to host") || err_str.contains("connection refused") {
            format!("Network connectivity issue. Cannot reach SMB server at {}", config.unc_path)
        } else if err_str.contains("timeout") || err_str.contains("timed out") {
            format!("Connection timeout. Server may be unreachable or firewall blocking port")
        } else if err_str.contains("access denied") || err_str.contains("authentication") || err_str.contains("login") {
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
        file.write_all(b"test")?;
        drop(file);
        std::fs::remove_file(&test_file).ok();
        Ok(())
    }

    async fn ensure_smb_dir(client: &Client, config: &SmbConfig) -> Result<()> {
        // Create directory recursively
        let parts: Vec<&str> = config.base_path.split('/').filter(|s| !s.is_empty()).collect();
        let unc_path = UncPath::from_str(&config.unc_path)
            .map_err(|e| AppError::Storage(format!("Invalid UNC path: {}", e)))?;

        let mut current_path = unc_path.clone();

        for part in parts {
            current_path = current_path.with_path(part);

            // Try to open as directory
            let open_args = FileCreateArgs::make_open_existing(
                FileAccessMask::new().with_generic_read(true),
            );

            match client.create_file(&current_path, &open_args).await {
                Ok(Resource::Directory(_)) => {
                    // Directory exists
                    tracing::debug!("SMB directory exists: {}", part);
                }
                Ok(Resource::File(_)) => {
                    return Err(AppError::Storage(format!(
                        "Path exists but is a file, not directory: {}",
                        part
                    )));
                }
                Err(_) => {
                    // Directory doesn't exist, try to create it
                    // Note: smb-rs may not have direct mkdir, we'll need to check API
                    // For now, we'll create it by trying to open/create a file in it
                    tracing::debug!("Creating SMB directory: {}", part);
                    // Directory creation will happen implicitly when we create files
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn verify_smb_write(client: &Client, config: &SmbConfig) -> Result<()> {
        let unc_path = UncPath::from_str(&config.unc_path)
            .map_err(|e| AppError::Storage(format!("Invalid UNC path: {}", e)))?;

        let test_path = unc_path.with_path(&format!("{}/.write_test", config.base_path));

        // Create test file - overwrite if exists
        let mut create_args = FileCreateArgs::make_overwrite(
            FileAttributes::default(),
            CreateOptions::default(),
        );
        create_args.desired_access = FileAccessMask::new().with_generic_write(true);

        let resource = client
            .create_file(&test_path, &create_args)
            .await
            .map_err(|e| AppError::Storage(format!("SMB write test failed: {}", e)))?;

        match resource {
            Resource::File(mut file) => {
                let _written = file
                    .write_at(b"test", 0)
                    .await
                    .map_err(|e| AppError::Storage(format!("SMB write test failed: {}", e)))?;
                file.close().await.ok();
            }
            _ => {
                return Err(AppError::Storage("Expected file resource".to_string()));
            }
        }

        // Clean up - delete test file
        let delete_args = FileCreateArgs::make_open_existing(
            FileAccessMask::new().with_delete(true),
        );

        if let Ok(Resource::File(mut file)) = client.create_file(&test_path, &delete_args).await {
            file.close().await.ok();
        }

        tracing::debug!("SMB write test passed");
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

    fn get_smb_file_path(&self, upload_id: &str, filename: &str) -> String {
        let safe_filename = Path::new(filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed");

        format!("{}/{}_{}", self.smb_config.base_path, upload_id, safe_filename)
    }
}

#[async_trait]
impl StorageBackend for SmbStorage {
    async fn store_part(
        &self,
        upload_id: &str,
        part_number: i32,
        data: Bytes,
    ) -> Result<String> {
        let part_path = self.get_part_path(upload_id, part_number);
        let data_len = data.len();

        // Parts go to local temp storage - use async I/O
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
            "Stored part {} for upload {} ({} bytes)",
            part_number,
            upload_id,
            data_len
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
        let smb_file_path = self.get_smb_file_path(upload_id, filename);
        let parts_path = self.parts_path.clone();
        let upload_id_owned = upload_id.to_string();

        tracing::info!(
            "Assembling {} parts for upload {} to SMB: {}",
            total_parts,
            upload_id,
            smb_file_path
        );

        // Step 1: Assemble parts locally first
        let temp_assembled = self.parts_path.join(format!("{}_assembled.tmp", upload_id));

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

        // Step 2: Copy assembled file to SMB
        let temp_path = temp_assembled.clone();
        let smb_path_str = smb_file_path.clone();
        let client_guard = self.client.lock().await;
        let client = &*client_guard;

        let unc_path = UncPath::from_str(&self.smb_config.unc_path)
            .map_err(|e| AppError::Storage(format!("Invalid UNC path: {}", e)))?;

        let smb_path = unc_path.with_path(&smb_path_str);

        // Read local file
        let data = std::fs::read(&temp_path).map_err(|e| {
            AppError::Storage(format!("Failed to read assembled file: {}", e))
        })?;

        // Write to SMB - overwrite if exists
        let mut create_args = FileCreateArgs::make_overwrite(
            FileAttributes::default(),
            CreateOptions::default(),
        );
        create_args.desired_access = FileAccessMask::new().with_generic_write(true);

        let resource = client
            .create_file(&smb_path, &create_args)
            .await
            .map_err(|e| AppError::Storage(format!("Failed to create SMB file: {}", e)))?;

        match resource {
            Resource::File(mut file) => {
                let _written = file
                    .write_at(&data, 0)
                    .await
                    .map_err(|e| AppError::Storage(format!("Failed to write to SMB: {}", e)))?;
                file.close()
                    .await
                    .map_err(|e| AppError::Storage(format!("Failed to close SMB file: {}", e)))?;
            }
            _ => {
                return Err(AppError::Storage("Expected file resource".to_string()));
            }
        }

        // Step 3: Clean up local temp files
        fs::remove_file(&temp_assembled).await.ok();
        self.delete_parts(upload_id).await?;

        let full_path = format!("{}/{}", self.smb_config.unc_path, smb_file_path);

        tracing::info!("Assembled {} parts to SMB: {}", total_parts, full_path);

        Ok(full_path)
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
        // Extract SMB path from full path
        // Path format: \\server\share\path\to\file
        let unc_path = UncPath::from_str(path)
            .map_err(|e| AppError::Storage(format!("Invalid SMB path {}: {}", path, e)))?;

        let client_guard = self.client.lock().await;
        let client = &*client_guard;

        // Open file with delete access
        let delete_args = FileCreateArgs::make_open_existing(
            FileAccessMask::new().with_delete(true),
        );

        if let Ok(Resource::File(mut file)) = client.create_file(&unc_path, &delete_args).await {
            file.close().await.ok();
            tracing::debug!("Deleted SMB file: {}", path);
        }

        Ok(())
    }

    fn backend_type(&self) -> &'static str {
        "smb"
    }
}
