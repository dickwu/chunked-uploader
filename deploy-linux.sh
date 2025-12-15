#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SERVICE_NAME="chunked-uploader"
SERVICE_FILE="/etc/systemd/system/$SERVICE_NAME.service"
USER_NAME="$(id -un)"

cd "$SCRIPT_DIR"

# --- Run mode (called by systemd with --run flag) ---
if [[ "$1" == "--run" ]]; then
    # Source the .env file
    set -a
    source "$SCRIPT_DIR/.env"
    set +a

    # Wait for mount point if LOCAL_STORAGE_PATH points to /mnt/ or /media/
    if [[ "$LOCAL_STORAGE_PATH" == /mnt/* ]] || [[ "$LOCAL_STORAGE_PATH" == /media/* ]]; then
        MOUNT_PATH=$(echo "$LOCAL_STORAGE_PATH" | cut -d'/' -f1-3)
        echo "Waiting for mount: $MOUNT_PATH"
        while [ ! -d "$MOUNT_PATH" ]; do
            sleep 5
        done
        echo "Mount $MOUNT_PATH is available"
    fi

    # Run the binary
    exec "$SCRIPT_DIR/target/release/chunked-uploader"
fi

# --- Deploy mode (default: create systemd service and enable) ---

# Check if running as root
if [[ $EUID -ne 0 ]]; then
    echo "Deploy mode requires root privileges. Please run with sudo."
    echo "Usage: sudo $0"
    exit 1
fi

# Stop existing service if running
systemctl stop "$SERVICE_NAME" 2>/dev/null
systemctl disable "$SERVICE_NAME" 2>/dev/null

# Create the systemd service file
cat > "$SERVICE_FILE" << EOF
[Unit]
Description=Chunked Uploader Service
After=network.target
After=local-fs.target

[Service]
Type=simple
User=$USER_NAME
WorkingDirectory=$SCRIPT_DIR
ExecStart=/bin/bash $SCRIPT_DIR/deploy-linux.sh --run
Restart=always
RestartSec=5
StandardOutput=append:$SCRIPT_DIR/chunked-uploader.stdout.log
StandardError=append:$SCRIPT_DIR/chunked-uploader.stderr.log

[Install]
WantedBy=multi-user.target
EOF

echo "Created systemd service: $SERVICE_FILE"

# Reload systemd daemon
systemctl daemon-reload

# Enable and start the service
systemctl enable "$SERVICE_NAME"
systemctl start "$SERVICE_NAME"

echo "Service enabled and started: $SERVICE_NAME"

# Check status
sleep 2
if systemctl is-active --quiet "$SERVICE_NAME"; then
    echo "Service is running"
    systemctl status "$SERVICE_NAME" --no-pager
else
    echo "Warning: Service may not have started correctly"
    systemctl status "$SERVICE_NAME" --no-pager
fi
