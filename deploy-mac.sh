#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOME_DIR="$(eval echo ~$(id -un))"
PLIST_NAME="com.grace.chunked-uploader"
PLIST_PATH="$HOME_DIR/Library/LaunchAgents/$PLIST_NAME.plist"

cd "$SCRIPT_DIR"

# --- Deploy mode ---

# Build the project
echo "Updating dependencies..."
cargo update

echo "Building release binary..."
# Include SMB support (pure Rust, no C dependencies needed)
cargo build --release --features smb
if [ $? -ne 0 ]; then
    echo "Build failed!"
    exit 1
fi
echo "Build successful"

# Create LaunchAgents directory if it doesn't exist
mkdir -p "$HOME_DIR/Library/LaunchAgents"

# Stop existing service if running
launchctl unload "$PLIST_PATH" 2>/dev/null
pkill -f "chunked-uploader" 2>/dev/null
sleep 1

# Load .env file to get environment variables
if [ -f "$SCRIPT_DIR/.env" ]; then
    set -a
    source "$SCRIPT_DIR/.env"
    set +a
fi

# Build EnvironmentVariables dict from .env file
ENV_DICT=""
while IFS= read -r line || [ -n "$line" ]; do
    # Skip comments and empty lines
    line=$(echo "$line" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
    [[ -z "$line" ]] && continue
    [[ "$line" =~ ^#.*$ ]] && continue
    
    # Split key and value
    key=$(echo "$line" | cut -d'=' -f1 | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
    value=$(echo "$line" | cut -d'=' -f2- | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
    
    # Skip if key is empty
    [[ -z "$key" ]] && continue
    
    # Remove quotes from value if present
    value=$(echo "$value" | sed -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//")
    
    # Escape special characters in value for XML
    value=$(echo "$value" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g; s/"/\&quot;/g')
    
    if [ -n "$ENV_DICT" ]; then
        ENV_DICT="$ENV_DICT
        <key>$key</key>
        <string>$value</string>"
    else
        ENV_DICT="        <key>$key</key>
        <string>$value</string>"
    fi
done < "$SCRIPT_DIR/.env"

# Create the launchd plist file - run binary directly with environment variables
cat > "$PLIST_PATH" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$PLIST_NAME</string>
    
    <key>ProgramArguments</key>
    <array>
        <string>$SCRIPT_DIR/target/release/chunked-uploader</string>
    </array>
    
    <key>WorkingDirectory</key>
    <string>$SCRIPT_DIR</string>
    
    <key>RunAtLoad</key>
    <true/>
    
    <key>KeepAlive</key>
    <true/>
    
    <key>ThrottleInterval</key>
    <integer>10</integer>
    
    <key>StandardOutPath</key>
    <string>$SCRIPT_DIR/chunked-uploader.stdout.log</string>
    
    <key>StandardErrorPath</key>
    <string>$SCRIPT_DIR/chunked-uploader.stderr.log</string>
    
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
        <key>HOME</key>
        <string>$HOME_DIR</string>
        <key>USER</key>
        <string>$(id -un)</string>
$ENV_DICT
    </dict>
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
    # Health check
    source "$SCRIPT_DIR/.env"
    PORT="${SERVER_PORT:-5001}"
    if curl -s "http://127.0.0.1:$PORT/health" | grep -q "OK"; then
        echo "Health check passed: http://127.0.0.1:$PORT"
    else
        echo "Warning: Health check failed"
    fi
else
    echo "Warning: Service may not have started correctly"
fi
