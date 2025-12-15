# Architecture Overview

## System Design

The Chunked Upload Server is designed to handle large file uploads (10GB+) through Cloudflare by splitting files into 50MB chunks. Each chunk is authenticated independently with JWT tokens, enabling parallel uploads and seamless resume capability.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                                  CLIENT                                      │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐        │
│  │  Chunk 0    │  │  Chunk 1    │  │  Chunk 2    │  │  Chunk N    │        │
│  │  (50MB)     │  │  (50MB)     │  │  (50MB)     │  │  (≤50MB)    │        │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘        │
│         │                │                │                │                │
│         │ JWT Token 0    │ JWT Token 1    │ JWT Token 2    │ JWT Token N   │
└─────────┼────────────────┼────────────────┼────────────────┼───────────────┘
          │                │                │                │
          ▼                ▼                ▼                ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                              CLOUDFLARE CDN                                  │
│                         (50MB request limit per chunk)                       │
└─────────────────────────────────────────────────────────────────────────────┘
          │                │                │                │
          ▼                ▼                ▼                ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                          CHUNKED UPLOAD SERVER                               │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                         Axum HTTP Router                               │  │
│  │  POST /upload/init     │  PUT /upload/{id}/part/{n}  │  GET /status   │  │
│  │  POST /upload/complete │  DELETE /upload/{id}        │  GET /health   │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                                    │                                         │
│  ┌─────────────────────────────────┼─────────────────────────────────────┐  │
│  │                                 ▼                                      │  │
│  │  ┌─────────────┐  ┌─────────────────────┐  ┌─────────────────────┐   │  │
│  │  │ API Key     │  │   JWT Validator     │  │  Request Handler    │   │  │
│  │  │ Middleware  │──▶  (per-part tokens)  │──▶  (business logic)   │   │  │
│  │  └─────────────┘  └─────────────────────┘  └──────────┬──────────┘   │  │
│  │                           AUTH LAYER                   │              │  │
│  └───────────────────────────────────────────────────────┼──────────────┘  │
│                                                          │                  │
│  ┌───────────────────────────────────────────────────────┼──────────────┐  │
│  │                                                       ▼               │  │
│  │  ┌─────────────────────────────────────────────────────────────────┐ │  │
│  │  │                      SQLite Database                             │ │  │
│  │  │  ┌─────────────┐              ┌──────────────────┐              │ │  │
│  │  │  │   uploads   │──────────────│   upload_parts   │              │ │  │
│  │  │  │  - id       │   1:N        │  - upload_id     │              │ │  │
│  │  │  │  - filename │              │  - part_number   │              │ │  │
│  │  │  │  - status   │              │  - status        │              │ │  │
│  │  │  │  - webhook  │              │  - token_hash    │              │ │  │
│  │  │  └─────────────┘              └──────────────────┘              │ │  │
│  │  └─────────────────────────────────────────────────────────────────┘ │  │
│  │                         PERSISTENCE LAYER                             │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                                    │                                         │
│  ┌─────────────────────────────────┼─────────────────────────────────────┐  │
│  │                                 ▼                                      │  │
│  │  ┌─────────────────────┐  ┌─────────────────────┐                    │  │
│  │  │   Local Storage     │  │    S3 Storage       │                    │  │
│  │  │   ./uploads/        │  │    s3://bucket/     │                    │  │
│  │  │   ├── parts/        │  │    ├── parts/       │                    │  │
│  │  │   │   └── {id}/     │  │    │   └── {id}/    │                    │  │
│  │  │   └── files/        │  │    └── files/       │                    │  │
│  │  └─────────────────────┘  └─────────────────────┘                    │  │
│  │                         STORAGE LAYER                                 │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Upload Flow

### Phase 1: Initialization

```
┌────────┐                      ┌────────┐                      ┌────────┐
│ Client │                      │ Server │                      │ SQLite │
└───┬────┘                      └───┬────┘                      └───┬────┘
    │                               │                               │
    │  POST /upload/init            │                               │
    │  {filename, size, webhook}    │                               │
    │──────────────────────────────▶│                               │
    │                               │                               │
    │                               │  Calculate parts count        │
    │                               │  (size / 50MB)                │
    │                               │                               │
    │                               │  Generate upload_id (UUID)    │
    │                               │                               │
    │                               │  For each part:               │
    │                               │    Generate JWT token         │
    │                               │    Store hash in DB           │
    │                               │                               │
    │                               │  INSERT uploads               │
    │                               │──────────────────────────────▶│
    │                               │                               │
    │                               │  INSERT upload_parts          │
    │                               │──────────────────────────────▶│
    │                               │                               │
    │  {file_id, parts: [          │                               │
    │    {part: 0, token: "..."},  │                               │
    │    {part: 1, token: "..."},  │                               │
    │    ...                       │                               │
    │  ]}                          │                               │
    │◀──────────────────────────────│                               │
    │                               │                               │
```

### Phase 2: Chunk Upload (Parallel)

```
┌────────┐                      ┌────────┐                      ┌─────────┐
│ Client │                      │ Server │                      │ Storage │
└───┬────┘                      └───┬────┘                      └────┬────┘
    │                               │                                │
    │  PUT /upload/{id}/part/0      │                                │
    │  Authorization: Bearer {jwt}  │                                │
    │  Body: <50MB binary>          │                                │
    │──────────────────────────────▶│                                │
    │                               │                                │
    │                               │  Validate JWT                  │
    │                               │  - Check signature             │
    │                               │  - Verify upload_id match      │
    │                               │  - Verify part_number match    │
    │                               │  - Check expiration            │
    │                               │                                │
    │                               │  Validate size matches claim   │
    │                               │                                │
    │                               │  Calculate SHA256 checksum     │
    │                               │                                │
    │                               │  Store chunk                   │
    │                               │────────────────────────────────▶
    │                               │                                │
    │                               │  Update part status            │
    │                               │  (pending → uploaded)          │
    │                               │                                │
    │  {part: 0, status: uploaded,  │                                │
    │   uploaded: 1, total: N}      │                                │
    │◀──────────────────────────────│                                │
    │                               │                                │
    │        ... parallel uploads for parts 1, 2, ... N ...          │
    │                               │                                │
```

### Phase 3: Completion

```
┌────────┐                      ┌────────┐        ┌─────────┐        ┌─────────┐
│ Client │                      │ Server │        │ Storage │        │ Webhook │
└───┬────┘                      └───┬────┘        └────┬────┘        └────┬────┘
    │                               │                  │                  │
    │  POST /upload/{id}/complete   │                  │                  │
    │  X-API-Key: {key}             │                  │                  │
    │──────────────────────────────▶│                  │                  │
    │                               │                  │                  │
    │                               │  Verify all parts uploaded         │
    │                               │                  │                  │
    │                               │  Assemble parts  │                  │
    │                               │─────────────────▶│                  │
    │                               │                  │                  │
    │                               │  For i in 0..N:  │                  │
    │                               │    Read part_i   │                  │
    │                               │    Append to final                 │
    │                               │    Delete part_i │                  │
    │                               │                  │                  │
    │                               │  Update status   │                  │
    │                               │  (pending → complete)              │
    │                               │                  │                  │
    │                               │  POST webhook (async)              │
    │                               │─────────────────────────────────────▶
    │                               │                  │                  │
    │  {status: complete,           │                  │                  │
    │   final_path: "..."}          │                  │                  │
    │◀──────────────────────────────│                  │                  │
    │                               │                  │                  │
```

## Data Model

### uploads Table

| Column | Type | Description |
|--------|------|-------------|
| id | TEXT (PK) | UUID v4 identifier |
| filename | TEXT | Original filename |
| total_size | INTEGER | Total file size in bytes |
| chunk_size | INTEGER | Size of each chunk (default 50MB) |
| total_parts | INTEGER | Number of chunks |
| status | TEXT | pending / complete / failed |
| storage_backend | TEXT | local / s3 |
| final_path | TEXT | Path to assembled file (after completion) |
| checksum_sha256 | TEXT | Optional file checksum |
| webhook_url | TEXT | URL to notify on completion |
| created_at | INTEGER | Unix timestamp |
| updated_at | INTEGER | Unix timestamp |
| expires_at | INTEGER | Unix timestamp for auto-cleanup |

### upload_parts Table

| Column | Type | Description |
|--------|------|-------------|
| upload_id | TEXT (FK) | Reference to uploads.id |
| part_number | INTEGER | 0-indexed part number |
| token_hash | TEXT | SHA256 hash of JWT token |
| status | TEXT | pending / uploaded |
| size | INTEGER | Expected size in bytes |
| checksum_sha256 | TEXT | Part checksum (after upload) |
| uploaded_at | INTEGER | Unix timestamp |

**Primary Key:** (upload_id, part_number)

## Security Model

### Authentication Layers

```
┌─────────────────────────────────────────────────────────────────┐
│                    AUTHENTICATION FLOW                          │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  Management Endpoints (init, status, complete, cancel)          │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  X-API-Key Header                                        │   │
│  │  - Static key from environment                           │   │
│  │  - Full access to upload management                      │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
│  Part Upload Endpoint (PUT /upload/{id}/part/{n})               │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  Authorization: Bearer {JWT}                             │   │
│  │  - Token generated at init time                          │   │
│  │  - Bound to specific upload_id + part_number             │   │
│  │  - Contains expected size for validation                 │   │
│  │  - Has expiration time                                   │   │
│  │  - Hash stored in DB for verification                    │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### JWT Token Structure

```json
{
  "upload_id": "550e8400-e29b-41d4-a716-446655440000",
  "part_number": 0,
  "expected_size": 52428800,
  "exp": 1734350400
}
```

**Security Properties:**
- Each part has a unique, non-reusable token
- Token is bound to specific upload and part number
- Size validation prevents chunk manipulation
- Expiration prevents indefinite token validity
- Token hash stored in DB for revocation capability

## Storage Backends

### Local Storage

```
./uploads/
├── parts/                      # Temporary chunk storage
│   ├── {upload_id_1}/
│   │   ├── part_000000
│   │   ├── part_000001
│   │   └── part_000002
│   └── {upload_id_2}/
│       └── part_000000
└── files/                      # Final assembled files
    ├── {upload_id_1}_{filename}
    └── {upload_id_2}_{filename}
```

### S3 Storage

```
s3://bucket/
├── parts/                      # Temporary chunk storage
│   ├── {upload_id_1}/
│   │   ├── part_000000
│   │   └── part_000001
│   └── {upload_id_2}/
│       └── part_000000
└── files/                      # Final assembled files
    └── {upload_id_1}_{filename}
```

Assembly uses S3 multipart upload with `UploadPartCopy` for efficient server-side concatenation.

## Background Services

### Cleanup Service

```
┌─────────────────────────────────────────────────────────────────┐
│                     CLEANUP SERVICE                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  Runs every: 1 hour                                             │
│                                                                 │
│  For each upload WHERE:                                         │
│    - status = 'pending'                                         │
│    - expires_at < NOW()                                         │
│                                                                 │
│  Actions:                                                       │
│    1. Delete all parts from storage                             │
│    2. Delete upload_parts records                               │
│    3. Delete uploads record                                     │
│                                                                 │
│  TTL configured via UPLOAD_TTL_HOURS (default: 24)              │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

## Error Handling

| Error Type | HTTP Status | Example |
|------------|-------------|---------|
| Unauthorized | 401 | Invalid/missing API key or JWT |
| BadRequest | 400 | Size mismatch, missing fields |
| NotFound | 404 | Upload or part not found |
| Conflict | 409 | Part already uploaded, upload complete |
| Internal | 500 | Database or storage errors |

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `API_KEY` | *required* | Management endpoint authentication |
| `JWT_SECRET` | *required* | JWT signing key |
| `STORAGE_BACKEND` | `local` | `local` or `s3` |
| `CHUNK_SIZE_MB` | `50` | Cloudflare-compatible default |
| `UPLOAD_TTL_HOURS` | `24` | Auto-cleanup threshold |
| `SERVER_PORT` | `3000` | HTTP listen port |

## Performance Considerations

1. **Parallel Uploads**: Clients can upload chunks in parallel (each has independent JWT)
2. **Streaming Assembly**: Parts are streamed during assembly to avoid memory pressure
3. **Connection Pooling**: SQLite uses r2d2 pool (10 connections)
4. **Async Webhooks**: Webhook calls don't block completion response
5. **Zero-Copy S3**: Uses `UploadPartCopy` for server-side assembly

