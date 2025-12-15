#!/bin/bash

# Chunked Upload Server - Environment Setup Script
# This script generates a .env file with secure random keys

set -e

ENV_FILE=".env"

# Generate random keys
generate_key() {
    openssl rand -hex 32 2>/dev/null || head -c 32 /dev/urandom | xxd -p -c 64
}

# Check if .env already exists
if [ -f "$ENV_FILE" ]; then
    read -p ".env file already exists. Overwrite? [y/N] " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        echo "Aborted."
        exit 0
    fi
fi

echo "Generating .env file..."

API_KEY=$(generate_key)
JWT_SECRET=$(generate_key)

cat > "$ENV_FILE" << EOF
# ===========================================
# Chunked Upload Server Configuration
# Generated on $(date)
# ===========================================

# Required - Authentication
API_KEY=${API_KEY}
JWT_SECRET=${JWT_SECRET}

# ===========================================
# Storage Configuration
# ===========================================

# Storage backend: "local" or "s3"
STORAGE_BACKEND=local

# Local storage path
LOCAL_STORAGE_PATH=./uploads

# S3 Configuration (uncomment if using S3)
# S3_ENDPOINT=https://s3.amazonaws.com
# S3_BUCKET=my-uploads-bucket
# S3_REGION=us-east-1
# AWS_ACCESS_KEY_ID=your-access-key
# AWS_SECRET_ACCESS_KEY=your-secret-key

# ===========================================
# Upload Settings
# ===========================================

# Chunk size in MB (default: 50MB for Cloudflare compatibility)
CHUNK_SIZE_MB=50

# How long uploads remain valid in hours
UPLOAD_TTL_HOURS=24

# ===========================================
# Server Settings
# ===========================================

# SQLite database path
DATABASE_PATH=./uploads.db

# Server port
SERVER_PORT=3000

# Logging level
RUST_LOG=chunked_uploader=info,tower_http=debug
EOF

# Create uploads directory
mkdir -p ./uploads

echo ""
echo "✓ .env file created successfully!"
echo "✓ uploads directory created"
echo ""
echo "Your API Key: ${API_KEY}"
echo ""
echo "To start the server:"
echo "  cargo run --release"
echo ""

