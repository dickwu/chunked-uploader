# Chunked Upload Server

A production-ready Rust HTTP server supporting **resumable chunked uploads** for large files (>10GB), designed for Cloudflare compatibility with 50MB chunk sizes.

## Features

- **Large File Support**: Upload files of any size (10GB+)
- **Resumable Uploads**: Continue interrupted uploads from where they left off
- **Cloudflare Compatible**: 50MB default chunk size fits within Cloudflare's request limits
- **JWT-based Part Authentication**: Each chunk has its own secure token
- **Multiple Storage Backends**: Local filesystem, SMB/NAS, or S3-compatible storage
- **Custom Paths**: Include path in filename (e.g., `videos/2024/movie.mp4`) to organize files
- **Auto Cleanup**: Expired incomplete uploads are automatically cleaned up
- **Async Finalization**: Non-blocking `/complete` with phase-aware status polling
- **Progress Tracking**: Real-time upload/finalization progress via SQLite persistence

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         Client                                  │
└─────────────────────────────────────────────────────────────────┘
                              │
              1. POST /upload/init (API Key)
              │ filename: "videos/2024/movie.mp4"
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
│     - Returns phase + upload/finalization progress               │
│                                                                 │
│  4. POST /upload/{id}/complete (API Key)                        │
│     - Starts async finalization (202 Accepted)                  │
│     - Poll /status until phase=complete or failed               │
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

**With custom path** (path is extracted from filename):
```bash
curl -X POST http://localhost:3000/upload/init \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-api-key" \
  -d '{
    "filename": "videos/2024/december/large-video.mp4",
    "total_size": 10737418240
  }'
```
This will store the file at `videos/2024/december/uuid_large-video.mp4`

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

### 5. Check Progress (for resume/finalization)

```bash
curl -X GET "http://localhost:3000/upload/${FILE_ID}/status?include_parts=true" \
  -H "X-API-Key: your-api-key"
```

Response:
```json
{
  "file_id": "550e8400-e29b-41d4-a716-446655440000",
  "filename": "large-video.mp4",
  "total_size": 10737418240,
  "chunk_size": 52428800,
  "total_parts": 205,
  "uploaded_parts": 100,
  "status": "pending",
  "phase": "uploading",
  "upload_progress_percent": 48.78,
  "finalizing_progress_percent": 0,
  "finalization_error": null,
  "storage_backend": "local",
  "parts": [
    {"part": 0, "status": "uploaded", "checksum_sha256": "...", "uploaded_at": "2026-02-07T04:20:00Z"},
    {"part": 1, "status": "pending", "checksum_sha256": null, "uploaded_at": null},
    ...
  ],
  "created_at": "2026-02-07T04:10:00Z",
  "expires_at": "2026-02-08T04:10:00Z",
  "final_path": null
}
```

### 6. Complete Upload

After all parts are uploaded, trigger finalization:

```bash
curl -X POST "http://localhost:3000/upload/${FILE_ID}/complete" \
  -H "X-API-Key: your-api-key"
```

Response while finalization is running (`202 Accepted`):
```json
{
  "file_id": "550e8400-e29b-41d4-a716-446655440000",
  "filename": "large-video.mp4",
  "total_size": 10737418240,
  "status": "finalizing",
  "phase": "finalizing",
  "final_path": null,
  "storage_backend": "local",
  "finalizing_progress_percent": 15
}
```

Poll `/upload/{id}/status` until terminal phase:

```bash
curl -X GET "http://localhost:3000/upload/${FILE_ID}/status" \
  -H "X-API-Key: your-api-key"
```

Completed status example:
```json
{
  "file_id": "550e8400-e29b-41d4-a716-446655440000",
  "filename": "large-video.mp4",
  "total_size": 10737418240,
  "status": "complete",
  "phase": "complete",
  "upload_progress_percent": 100,
  "finalizing_progress_percent": 100,
  "finalization_error": null,
  "final_path": "./uploads/550e8400..._large-video.mp4",
  "storage_backend": "local"
}
```

### 7. Cancel Upload (optional)

```bash
curl -X DELETE "http://localhost:3000/upload/${FILE_ID}" \
  -H "X-API-Key: your-api-key"
```

Note: cancel returns `409 Conflict` while `status=finalizing`.

## API Reference

| Endpoint | Method | Auth | Description |
|----------|--------|------|-------------|
| `/upload/init` | POST | API Key | Initialize upload, get part tokens |
| `/upload/{id}/part/{n}` | PUT | JWT (per part) | Upload a single chunk |
| `/upload/{id}/status` | GET | API Key | Get upload + finalization status (`include_parts` optional) |
| `/upload/{id}/complete` | POST | API Key | Start/check async finalization (`202` while running, `200` when complete) |
| `/upload/{id}` | DELETE | API Key | Cancel and cleanup (`409` if finalizing) |
| `/health` | GET | None | Health check |

### Status Lifecycle

- `status`: `pending | finalizing | complete | failed`
- `phase`: `uploading | finalizing | complete | failed`
- `upload_progress_percent` tracks chunk upload progress (0-100)
- `finalizing_progress_percent` tracks backend finalization progress (0-100)

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `API_KEY` | *required* | API key for authentication |
| `JWT_SECRET` | *required* | Secret for JWT token signing |
| `STORAGE_BACKEND` | `local` | `local`, `smb`, or `s3` |
| `LOCAL_STORAGE_PATH` | `./uploads` | Path for local storage |
| `TEMP_STORAGE_PATH` | system temp | Local path for temporary chunk storage (fast SSD recommended). Used by S3 and SMB backends. |
| `SMB_HOST` | `localhost` | SMB server hostname or IP |
| `SMB_PORT` | `445` | SMB server port |
| `SMB_USER` | | SMB username |
| `SMB_PASS` | | SMB password |
| `SMB_SHARE` | `share` | SMB share name |
| `SMB_PATH` | | Subdirectory within the share (optional) |
| `SMB_MOUNT_POINT` | `/Volumes/uploads` | SMB mount point (for operational compatibility/scripts) |
| `S3_ENDPOINT` | AWS default | S3 endpoint URL |
| `S3_BUCKET` | `uploads` | S3 bucket name |
| `S3_REGION` | `us-east-1` | S3 region |
| `CHUNK_SIZE_MB` | `50` | Chunk size in MB |
| `UPLOAD_TTL_HOURS` | `24` | Hours before incomplete uploads expire |
| `MAX_CONCURRENT_FINALIZATIONS` | `4` | Limit of concurrent finalization jobs |
| `DATABASE_PATH` | `./uploads.db` | SQLite database path |
| `SERVER_PORT` | `3000` | Server port |

## Resume Upload Flow

1. Client starts upload with `POST /upload/init`
2. Client uploads chunks in parallel or sequence
3. If interrupted, client calls `GET /upload/{id}/status?include_parts=true`
4. Response shows which parts are `pending` vs `uploaded`
5. Client re-uploads only `pending` parts using original tokens
6. When all parts uploaded, call `POST /upload/{id}/complete`
7. Poll `GET /upload/{id}/status` until `phase=complete` (or handle `phase=failed`)

## Example Client (Python)

```python
import requests
import os
import time

API_KEY = "your-api-key"
BASE_URL = "http://localhost:3000"
FILE_PATH = "large-file.zip"
CHUNK_SIZE = 50 * 1024 * 1024  # 50MB

def upload_file(file_path, target_path=None):
    """
    Upload a file to the chunked upload server.
    
    Args:
        file_path: Local path to the file
        target_path: Optional remote path (e.g., "videos/2024")
    """
    file_size = os.path.getsize(file_path)
    filename = os.path.basename(file_path)
    
    # Include target path in filename if specified
    remote_filename = f"{target_path}/{filename}" if target_path else filename
    
    # 1. Initialize upload
    resp = requests.post(
        f"{BASE_URL}/upload/init",
        headers={"X-API-Key": API_KEY},
        json={"filename": remote_filename, "total_size": file_size}
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
    
    # 3. Trigger async finalization
    resp = requests.post(
        f"{BASE_URL}/upload/{file_id}/complete",
        headers={"X-API-Key": API_KEY}
    )
    resp.raise_for_status()

    # 4. Poll status until finalization is complete
    while True:
        status_resp = requests.get(
            f"{BASE_URL}/upload/{file_id}/status",
            headers={"X-API-Key": API_KEY}
        )
        status_resp.raise_for_status()
        status = status_resp.json()

        phase = status.get("phase")
        if phase == "complete":
            print(f"Upload complete: {status['final_path']}")
            break
        if phase == "failed":
            raise RuntimeError(f"Finalization failed: {status.get('finalization_error')}")

        print(f"Finalizing... {status.get('finalizing_progress_percent', 0)}%")
        time.sleep(2)

if __name__ == "__main__":
    # Simple upload (file goes to default location)
    upload_file(FILE_PATH)
    
    # Upload to specific path
    upload_file(FILE_PATH, target_path="videos/2024/december")
```

## JavaScript/TypeScript SDK

Official SDK for browser and Node.js: [`chunked-uploader-sdk`](https://www.npmjs.com/package/chunked-uploader-sdk)

### Features

- **Large File Support**: Upload files of any size (10GB+)
- **Automatic Chunking**: Files split into 50MB chunks (Cloudflare compatible)
- **Parallel Uploads**: Configurable concurrency for faster uploads
- **Resumable**: Continue interrupted uploads from where they left off
- **Phase Progress Tracking**: `uploading -> finalizing -> complete`
- **Retry Logic**: Automatic retry for failed chunks
- **TypeScript**: Full type definitions included
- **Isomorphic**: Works in both browser and Node.js

### Installation

```bash
npm install chunked-uploader-sdk
```

### Quick Start

```typescript
import { ChunkedUploader } from 'chunked-uploader-sdk';

const uploader = new ChunkedUploader({
  baseUrl: 'https://upload.example.com',
  apiKey: 'your-api-key',
});

// Upload a file with progress tracking
const result = await uploader.uploadFile(file, {
  onProgress: (event) => {
    console.log(`Progress: ${event.overallProgress.toFixed(1)}%`);
  },
});

if (result.success) {
  console.log('Upload complete:', result.finalPath);
} else {
  console.error('Upload failed:', result.error);
}
```

### Browser Example

```typescript
const uploader = new ChunkedUploader({
  baseUrl: 'http://localhost:3000',
  apiKey: 'your-api-key',
});

// File input handler
const input = document.querySelector('input[type="file"]') as HTMLInputElement;
input.addEventListener('change', async () => {
  const file = input.files?.[0];
  if (!file) return;

  const result = await uploader.uploadFile(file, {
    concurrency: 5, // Upload 5 parts simultaneously
    onProgress: (event) => {
      progressBar.style.width = `${event.phaseProgress}%`;
      statusText.textContent =
        event.phase === 'uploading'
          ? `Uploading ${event.uploadProgress.toFixed(0)}%`
          : event.phase === 'finalizing'
            ? `Finalizing ${event.finalizingProgress.toFixed(0)}%`
            : 'Complete';
    },
    onPartComplete: (result) => {
      if (!result.success) {
        console.error(`Part ${result.partNumber} failed:`, result.error);
      }
    },
  });

  console.log(result);
});
```

### Resume Interrupted Upload

```typescript
// Store part tokens from initial upload
const tokenMap = new Map<number, string>();
initResponse.parts.forEach(p => tokenMap.set(p.part, p.token));

// Later, resume the upload
const result = await uploader.resumeUpload(uploadId, file, {
  partTokens: tokenMap,
  onProgress: (event) => console.log(`${event.overallProgress}%`),
});
```

### Cancellable Upload

```typescript
const abortController = new AbortController();

// Cancel button
cancelButton.addEventListener('click', () => {
  abortController.abort();
});

const result = await uploader.uploadFile(file, {
  signal: abortController.signal,
});

if (!result.success && result.error?.message === 'Upload aborted') {
  console.log('Upload was cancelled');
}
```

### Node.js Usage

```typescript
import { ChunkedUploader } from 'chunked-uploader-sdk';
import { readFile } from 'fs/promises';

const uploader = new ChunkedUploader({
  baseUrl: 'http://localhost:3000',
  apiKey: 'your-api-key',
  concurrency: 5,
});

async function uploadFromDisk(filePath: string) {
  const buffer = await readFile(filePath);
  const result = await uploader.uploadFile(buffer, {
    onProgress: (event) => {
      process.stdout.write(`\rProgress: ${event.overallProgress.toFixed(1)}%`);
    },
  });
  console.log('\nUpload complete:', result);
}
```

### Configuration Options

```typescript
interface ChunkedUploaderConfig {
  /** Base URL of the chunked upload server */
  baseUrl: string;
  /** API key for management endpoints */
  apiKey: string;
  /** Request timeout in milliseconds (default: 30000) */
  timeout?: number;
  /** Number of concurrent chunk uploads (default: 3) */
  concurrency?: number;
  /** Retry attempts for failed chunks (default: 3) */
  retryAttempts?: number;
  /** Delay between retries in milliseconds (default: 1000) */
  retryDelay?: number;
  /** Poll interval while waiting for finalization (default: 2000) */
  finalizePollIntervalMs?: number;
  /** Finalization timeout in milliseconds (default: 7200000 / 2h) */
  finalizeTimeoutMs?: number;
  /** Custom fetch implementation */
  fetch?: typeof fetch;
}
```

### SDK Methods

| Method | Description |
|--------|-------------|
| `uploadFile(file, options?)` | Upload a file with automatic chunking and parallel uploads |
| `resumeUpload(uploadId, file, options?)` | Resume an interrupted upload |
| `initUpload(filename, totalSize, webhookUrl?)` | Initialize an upload session manually |
| `uploadPart(uploadId, partNumber, token, data, signal?)` | Upload a single chunk |
| `getStatus(uploadId, options?)` | Get upload/finalization status (`includeParts` optional) |
| `completeUpload(uploadId)` | Trigger async finalization and wait for completion |
| `cancelUpload(uploadId)` | Cancel an upload and cleanup |
| `healthCheck()` | Check server health |

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
  "final_path": "./uploads/550e8400..._large-video.mp4",
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

# Optional: Fast local storage for temporary chunks (recommended)
TEMP_STORAGE_PATH=/tmp/chunked-uploads
```

### S3 Storage Architecture

The S3 backend uses a hybrid approach for optimal performance:

1. **Chunks are stored locally** on fast storage (SSD) during upload
2. **Final assembled file is uploaded to S3** after all chunks complete
3. **Automatic cleanup** of local temporary files

This design ensures:
- Fast chunk uploads (no network latency per chunk)
- No S3 multipart upload complexity or minimum part size restrictions
- Reliable large file transfers to S3
- Works with any S3-compatible storage (AWS S3, MinIO, Cloudflare R2, etc.)

### Building with S3 Support

```bash
# Build with S3 feature
cargo build --release --features s3
```

### Running S3 Tests

```bash
# Ensure .env has S3 credentials configured
# Start server with S3 backend
cargo run --features s3

# Run integration tests (in another terminal)
cargo test --features s3 --test s3_upload_test -- --nocapture --test-threads=1
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

# Optional: local temp storage (used for compatibility cleanup paths)
TEMP_STORAGE_PATH=/tmp/chunked-uploads
```

### SMB Storage Architecture

The SMB backend uses direct writes with async finalization:

1. **Each uploaded chunk is written directly to an SMB `.partial` file** at the correct offset
2. **`POST /complete` starts finalization** (verification + rename `.partial` to final file)
3. **Automatic cleanup/recovery** for stale incomplete/finalizing uploads

This design ensures:
- No long synchronous assembly step at 100% upload
- Reliable large file finalization with observable progress
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
