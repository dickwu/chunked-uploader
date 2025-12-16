# Chunked Upload Server

A production-ready Rust HTTP server supporting **resumable chunked uploads** for large files (>10GB), designed for Cloudflare compatibility with 50MB chunk sizes.

## Features

- **Large File Support**: Upload files of any size (10GB+)
- **Resumable Uploads**: Continue interrupted uploads from where they left off
- **Cloudflare Compatible**: 50MB default chunk size fits within Cloudflare's request limits
- **JWT-based Part Authentication**: Each chunk has its own secure token
- **Multiple Storage Backends**: Local filesystem, SMB/NAS, or S3-compatible storage
- **Auto Cleanup**: Expired incomplete uploads are automatically cleaned up
- **Progress Tracking**: Real-time upload progress via SQLite persistence

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         Client                                  │
└─────────────────────────────────────────────────────────────────┘
                              │
              1. POST /upload/init (API Key)
              │ Returns: file_id + JWT tokens for each part
              ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Upload Server (Rust/Axum)                    │
├─────────────────────────────────────────────────────────────────┤
│  2. PUT /upload/{id}/part/{n} (JWT Token per part)              │
│     - Validates token                                           │
│     - Stores chunk                                              │
│     - Updates SQLite                                            │
│                                                                 │
│  3. GET /upload/{id}/status (API Key)                           │
│     - Returns progress: [{part: 0, status: "uploaded"}, ...]    │
│                                                                 │
│  4. POST /upload/{id}/complete (API Key)                        │
│     - Assembles all parts                                       │
│     - Returns final file path                                   │
└─────────────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
┌─────────────────────┐ ┌─────────────────┐ ┌─────────────────────┐
│   Local Storage     │ │   SMB/NAS       │ │    S3 Storage       │
│   ./uploads/        │ │   \\server\share│ │    s3://bucket/     │
└─────────────────────┘ └─────────────────┘ └─────────────────────┘
```

## Quick Start

### 1. Setup

```bash
# Initialize environment (generates .env with secure random keys)
./init.sh

# Edit .env if needed (e.g., change storage path, port)
nano .env

# Build
cargo build --release
```

### 2. Deploy as Service

#### macOS (launchd)

```bash
# Deploy and start service (creates LaunchAgent and loads it)
./deploy-mac.sh

# Service management
launchctl list | grep chunked-uploader    # Check status
launchctl unload ~/Library/LaunchAgents/com.grace.chunked-uploader.plist  # Stop
launchctl load ~/Library/LaunchAgents/com.grace.chunked-uploader.plist    # Start

# View logs
tail -f chunked-uploader.stdout.log
```

#### Linux (systemd)

```bash
# Deploy and start service (requires sudo)
sudo ./deploy-linux.sh

# Service management
sudo systemctl status chunked-uploader    # Check status
sudo systemctl restart chunked-uploader   # Restart
sudo systemctl stop chunked-uploader      # Stop
sudo systemctl enable chunked-uploader    # Enable on boot

# View logs
sudo journalctl -u chunked-uploader -f
# or
tail -f chunked-uploader.stdout.log
```

#### Manual Run (Development)

```bash
./target/release/chunked-uploader
```

### 3. Initialize Upload

```bash
curl -X POST http://localhost:3000/upload/init \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-api-key" \
  -d '{
    "filename": "large-video.mp4",
    "total_size": 10737418240,
    "webhook_url": "https://your-server.com/webhook/upload-complete"
  }'
```

Response:
```json
{
  "file_id": "550e8400-e29b-41d4-a716-446655440000",
  "total_parts": 205,
  "chunk_size": 52428800,
  "parts": [
    {"part": 0, "token": "eyJhbGc...", "status": "pending"},
    {"part": 1, "token": "eyJhbGc...", "status": "pending"},
    ...
  ],
  "expires_at": "2025-12-16T12:00:00Z"
}
```

### 4. Upload Parts

Upload each 50MB chunk with its corresponding JWT token:

```bash
# Upload part 0
curl -X PUT "http://localhost:3000/upload/${FILE_ID}/part/0" \
  -H "Authorization: Bearer ${PART_0_TOKEN}" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @chunk_0.bin
```

Response:
```json
{
  "upload_id": "550e8400-e29b-41d4-a716-446655440000",
  "part_number": 0,
  "status": "uploaded",
  "checksum_sha256": "abc123...",
  "uploaded_parts": 1,
  "total_parts": 205
}
```

### 5. Check Progress (for resume)

```bash
curl -X GET "http://localhost:3000/upload/${FILE_ID}/status" \
  -H "X-API-Key: your-api-key"
```

Response:
```json
{
  "file_id": "550e8400-e29b-41d4-a716-446655440000",
  "filename": "large-video.mp4",
  "total_size": 10737418240,
  "total_parts": 205,
  "uploaded_parts": 100,
  "progress_percent": 48.78,
  "parts": [
    {"part": 0, "status": "uploaded", "checksum_sha256": "..."},
    {"part": 1, "status": "pending", "checksum_sha256": null},
    ...
  ]
}
```

### 6. Complete Upload

After all parts are uploaded:

```bash
curl -X POST "http://localhost:3000/upload/${FILE_ID}/complete" \
  -H "X-API-Key: your-api-key"
```

Response:
```json
{
  "file_id": "550e8400-e29b-41d4-a716-446655440000",
  "filename": "large-video.mp4",
  "total_size": 10737418240,
  "status": "complete",
  "final_path": "./uploads/files/550e8400..._large-video.mp4",
  "storage_backend": "local"
}
```

### 7. Cancel Upload (optional)

```bash
curl -X DELETE "http://localhost:3000/upload/${FILE_ID}" \
  -H "X-API-Key: your-api-key"
```

## API Reference

| Endpoint | Method | Auth | Description |
|----------|--------|------|-------------|
| `/upload/init` | POST | API Key | Initialize upload, get part tokens |
| `/upload/{id}/part/{n}` | PUT | JWT (per part) | Upload a single chunk |
| `/upload/{id}/status` | GET | API Key | Get upload progress |
| `/upload/{id}/complete` | POST | API Key | Assemble all parts |
| `/upload/{id}` | DELETE | API Key | Cancel and cleanup |
| `/health` | GET | None | Health check |

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `API_KEY` | *required* | API key for authentication |
| `JWT_SECRET` | *required* | Secret for JWT token signing |
| `STORAGE_BACKEND` | `local` | `local`, `smb`, or `s3` |
| `LOCAL_STORAGE_PATH` | `./uploads` | Path for local storage |
| `TEMP_STORAGE_PATH` | system temp | Local path for temporary chunk storage (fast SSD recommended) |
| `SMB_HOST` | `localhost` | SMB server hostname or IP |
| `SMB_PORT` | `445` | SMB server port |
| `SMB_USER` | | SMB username |
| `SMB_PASS` | | SMB password |
| `SMB_SHARE` | `share` | SMB share name |
| `SMB_PATH` | | Subdirectory within the share (optional) |
| `S3_ENDPOINT` | AWS default | S3 endpoint URL |
| `S3_BUCKET` | `uploads` | S3 bucket name |
| `S3_REGION` | `us-east-1` | S3 region |
| `CHUNK_SIZE_MB` | `50` | Chunk size in MB |
| `UPLOAD_TTL_HOURS` | `24` | Hours before incomplete uploads expire |
| `DATABASE_PATH` | `./uploads.db` | SQLite database path |
| `SERVER_PORT` | `3000` | Server port |

## Resume Upload Flow

1. Client starts upload with `POST /upload/init`
2. Client uploads chunks in parallel or sequence
3. If interrupted, client calls `GET /upload/{id}/status`
4. Response shows which parts are `pending` vs `uploaded`
5. Client re-uploads only `pending` parts using original tokens
6. When all parts uploaded, call `POST /upload/{id}/complete`

## Example Client (Python)

```python
import requests
import os

API_KEY = "your-api-key"
BASE_URL = "http://localhost:3000"
FILE_PATH = "large-file.zip"
CHUNK_SIZE = 50 * 1024 * 1024  # 50MB

def upload_file(file_path):
    file_size = os.path.getsize(file_path)
    filename = os.path.basename(file_path)
    
    # 1. Initialize upload
    resp = requests.post(
        f"{BASE_URL}/upload/init",
        headers={"X-API-Key": API_KEY},
        json={"filename": filename, "total_size": file_size}
    )
    data = resp.json()
    file_id = data["file_id"]
    parts = data["parts"]
    
    print(f"Upload initialized: {file_id}, {len(parts)} parts")
    
    # 2. Upload each part
    with open(file_path, "rb") as f:
        for part_info in parts:
            part_num = part_info["part"]
            token = part_info["token"]
            
            # Read chunk
            chunk = f.read(CHUNK_SIZE)
            if not chunk:
                break
            
            # Upload
            resp = requests.put(
                f"{BASE_URL}/upload/{file_id}/part/{part_num}",
                headers={"Authorization": f"Bearer {token}"},
                data=chunk
            )
            
            result = resp.json()
            print(f"Part {part_num}: {result['uploaded_parts']}/{result['total_parts']}")
    
    # 3. Complete upload
    resp = requests.post(
        f"{BASE_URL}/upload/{file_id}/complete",
        headers={"X-API-Key": API_KEY}
    )
    
    print(f"Upload complete: {resp.json()['final_path']}")

if __name__ == "__main__":
    upload_file(FILE_PATH)
```

## Scripts Reference

| Script | Description |
|--------|-------------|
| `init.sh` | Generates `.env` file with secure random API_KEY and JWT_SECRET |
| `deploy-mac.sh` | Creates macOS LaunchAgent and starts service (auto-restarts on reboot) |
| `deploy-linux.sh` | Creates systemd service and starts it (requires sudo, auto-restarts on reboot) |

### init.sh

Initializes the environment configuration:
- Generates cryptographically secure `API_KEY` and `JWT_SECRET`
- Creates `.env` file with default settings
- Creates `uploads` directory

```bash
./init.sh
```

### deploy-mac.sh

Deploys on macOS using launchd:
- Creates `~/Library/LaunchAgents/com.grace.chunked-uploader.plist`
- Waits for external volumes (e.g., `/Volumes/...`) to mount before starting
- Auto-restarts if the process crashes
- Starts automatically on login

```bash
./deploy-mac.sh           # Deploy and start
./deploy-mac.sh --run     # Run mode (used by launchd internally)
```

### deploy-linux.sh

Deploys on Linux using systemd:
- Creates `/etc/systemd/system/chunked-uploader.service`
- Waits for mount points (e.g., `/mnt/...`, `/media/...`) before starting
- Auto-restarts if the process crashes
- Starts automatically on boot

```bash
sudo ./deploy-linux.sh           # Deploy and start
./deploy-linux.sh --run          # Run mode (used by systemd internally)
```

## Building

```bash
# Default build (local storage only)
cargo build --release

# With SMB/NAS support (pure Rust, no external dependencies)
cargo build --release --features smb

# With S3 support (requires native crypto libs)
cargo build --release --features s3

# With both SMB and S3 support
cargo build --release --features "smb s3"

# The binary will be at:
./target/release/chunked-uploader

# Run with custom config
API_KEY=xxx JWT_SECRET=yyy ./target/release/chunked-uploader
```

### Build Requirements

- Rust 1.70+
- SQLite development libraries (usually bundled)
- For S3 feature: native crypto libraries (OpenSSL or aws-lc)

## Webhook Notifications

When initializing an upload, you can provide a `webhook_url`. When the upload completes, the server will POST a notification to that URL:

```json
{
  "event": "upload.complete",
  "file_id": "550e8400-e29b-41d4-a716-446655440000",
  "filename": "large-video.mp4",
  "total_size": 10737418240,
  "final_path": "./uploads/files/550e8400..._large-video.mp4",
  "storage_backend": "local",
  "completed_at": "2025-12-15T10:30:00Z"
}
```

The webhook is called asynchronously and does not block the complete response.

## S3 Storage Setup

For S3-compatible storage (AWS S3, MinIO, etc.):

```bash
# .env
STORAGE_BACKEND=s3
S3_ENDPOINT=https://play.min.io  # or AWS endpoint
S3_BUCKET=my-uploads
S3_REGION=us-east-1
AWS_ACCESS_KEY_ID=your-access-key
AWS_SECRET_ACCESS_KEY=your-secret-key
```

## SMB/NAS Storage Setup

For SMB/CIFS network storage (NAS devices, Windows shares, Samba):

```bash
# .env
STORAGE_BACKEND=smb
SMB_HOST=192.168.1.100        # NAS IP or hostname
SMB_PORT=445                   # Default SMB port
SMB_USER=admin                 # SMB username
SMB_PASS=your-password         # SMB password
SMB_SHARE=uploads              # Share name on the server
SMB_PATH=videos                # Optional: subdirectory within share

# Optional: Fast local storage for temporary chunks (recommended)
TEMP_STORAGE_PATH=/tmp/chunked-uploads
```

### SMB Storage Architecture

The SMB backend uses a hybrid approach for optimal performance:

1. **Chunks are stored locally** on fast storage (SSD) during upload
2. **Final assembled file is transferred to SMB** after all chunks complete
3. **Automatic cleanup** of local temporary files

This design ensures:
- Fast chunk uploads (no network latency per chunk)
- Reliable large file transfers to NAS
- Works with any SMB 3.0+ compatible server (Synology, QNAP, TrueNAS, Windows, Samba)

### Building with SMB Support

```bash
# Build with SMB feature (pure Rust, no external dependencies)
cargo build --release --features smb
```

### macOS Local Network Permission

On macOS Sequoia (15.x) and later, apps need permission to access local network resources. When deploying with `deploy-mac.sh`, the service may need Local Network permission:

1. Run the binary once manually to trigger the permission prompt:
   ```bash
   source .env && ./target/release/chunked-uploader
   ```
2. If prompted, allow "Local Network" access in System Settings > Privacy & Security > Local Network
3. Then deploy normally with `./deploy-mac.sh`

### Troubleshooting SMB Connection

If SMB connection fails:

```bash
# Test network connectivity
ping 192.168.1.100

# Test SMB port
nc -zv 192.168.1.100 445

# Test SMB connection (on macOS/Linux)
smbclient //192.168.1.100/share -U username

# Check server logs
tail -f chunked-uploader.stderr.log
```

Common issues:
- **"No route to host"**: Network/firewall issue or macOS Local Network permission needed
- **"Access denied"**: Check username/password
- **"Share not found"**: Verify share name exists on server

## License

MIT

