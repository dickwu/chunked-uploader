#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOME_DIR="$(eval echo ~$(id -un))"
PLIST_NAME="com.grace.chunked-uploader"
PLIST_PATH="$HOME_DIR/Library/LaunchAgents/$PLIST_NAME.plist"

cd "$SCRIPT_DIR"

# --- Run mode (called by launchd with --run flag) ---
if [[ "$1" == "--run" ]]; then
    # Source the .env file
    set -a
    source "$SCRIPT_DIR/.env"
    set +a

    # Wait for external volume if LOCAL_STORAGE_PATH points to /Volumes/
    if [[ "$LOCAL_STORAGE_PATH" == /Volumes/* ]]; then
        VOLUME_PATH=$(echo "$LOCAL_STORAGE_PATH" | cut -d'/' -f1-3)
        echo "Waiting for volume: $VOLUME_PATH"
        while [ ! -d "$VOLUME_PATH" ]; do
            sleep 5
        done
        echo "Volume $VOLUME_PATH is mounted"
    fi

    # Run the binary
    exec "$SCRIPT_DIR/target/release/chunked-uploader"
fi

# --- Deploy mode (default: create plist and load service) ---

# Create LaunchAgents directory if it doesn't exist
mkdir -p "$HOME_DIR/Library/LaunchAgents"

# Unload existing service if running
launchctl unload "$PLIST_PATH" 2>/dev/null

# Create the launchd plist file
cat > "$PLIST_PATH" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$PLIST_NAME</string>
    
    <key>ProgramArguments</key>
    <array>
        <string>/bin/bash</string>
        <string>$SCRIPT_DIR/deploy-mac.sh</string>
        <string>--run</string>
    </array>
    
    <key>WorkingDirectory</key>
    <string>$SCRIPT_DIR</string>
    
    <key>RunAtLoad</key>
    <true/>
    
    <key>KeepAlive</key>
    <true/>
    
    <key>StandardOutPath</key>
    <string>$SCRIPT_DIR/chunked-uploader.stdout.log</string>
    
    <key>StandardErrorPath</key>
    <string>$SCRIPT_DIR/chunked-uploader.stderr.log</string>
</dict>
</plist>
EOF

echo "Created launchd plist: $PLIST_PATH"

# Load the service
launchctl load "$PLIST_PATH"
echo "Service loaded: $PLIST_NAME"

# Check status
sleep 2
if launchctl list | grep -q "$PLIST_NAME"; then
    echo "Service is running"
else
    echo "Warning: Service may not have started correctly"
fi
