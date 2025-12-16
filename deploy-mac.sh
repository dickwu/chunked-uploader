#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOME_DIR="$(eval echo ~$(id -un))"
PLIST_NAME="com.grace.chunked-uploader"
PLIST_PATH="$HOME_DIR/Library/LaunchAgents/$PLIST_NAME.plist"
APP_NAME="ChunkedUploader"
APP_PATH="$SCRIPT_DIR/$APP_NAME.app"

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
pkill -f "$APP_NAME" 2>/dev/null
sleep 1

# Load .env file to get environment variables
if [ -f "$SCRIPT_DIR/.env" ]; then
    set -a
    source "$SCRIPT_DIR/.env"
    set +a
fi

# --- Create App Bundle ---
# macOS Tahoe (26.x) has stricter Local Network privacy controls that block
# launchd-spawned processes from accessing local network IPs.
# Using an app bundle with `open -W -a` allows the process to run in the 
# GUI session context with proper network permissions.

echo "Creating app bundle: $APP_PATH"

# Remove old app bundle if exists
rm -rf "$APP_PATH"

# Create app bundle structure
mkdir -p "$APP_PATH/Contents/MacOS"
mkdir -p "$APP_PATH/Contents/Resources"

# Build environment variables for LSEnvironment in Info.plist
ENV_PLIST=""
if [ -f "$SCRIPT_DIR/.env" ]; then
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
        
        # Convert relative paths to absolute (for DATABASE_PATH and similar)
        if [[ "$value" == ./* ]]; then
            value="$SCRIPT_DIR/${value#./}"
        fi
        
        # Escape special characters in value for XML
        value=$(echo "$value" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g; s/"/\&quot;/g')
        
        ENV_PLIST="${ENV_PLIST}
        <key>$key</key>
        <string>$value</string>"
    done < "$SCRIPT_DIR/.env"
fi

# Create Info.plist for the app bundle with LSEnvironment
cat > "$APP_PATH/Contents/Info.plist" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>com.grace.chunked-uploader</string>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>LSBackgroundOnly</key>
    <true/>
    <key>LSUIElement</key>
    <true/>
    <key>NSLocalNetworkUsageDescription</key>
    <string>ChunkedUploader needs local network access to serve files.</string>
    <key>NSBonjourServices</key>
    <array>
        <string>_http._tcp</string>
    </array>
    <key>LSEnvironment</key>
    <dict>$ENV_PLIST
    </dict>
</dict>
</plist>
EOF

# Copy the actual binary to the app bundle
cp "$SCRIPT_DIR/target/release/chunked-uploader" "$APP_PATH/Contents/MacOS/$APP_NAME"
chmod +x "$APP_PATH/Contents/MacOS/$APP_NAME"

echo "App bundle created successfully"

# Build environment variables string for the launcher script
ENV_OPEN_ARGS=""
if [ -f "$SCRIPT_DIR/.env" ]; then
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
        
        # Build --env arguments for open command
        ENV_OPEN_ARGS="${ENV_OPEN_ARGS} --env ${key}=${value}"
    done < "$SCRIPT_DIR/.env"
fi

# Create a standalone launcher script that will be executed by launchd
# This script uses `open -W -a` to launch the app in GUI session context
LAUNCHER_SCRIPT="$SCRIPT_DIR/run-chunked-uploader.sh"
cat > "$LAUNCHER_SCRIPT" << EOF
#!/bin/bash
# Launcher script for ChunkedUploader
# Uses open -W -a to launch in GUI context
# Environment variables are embedded in the app's Info.plist via LSEnvironment

cd "$SCRIPT_DIR"

# Launch the app using open -W -a
# This runs the process in GUI session context, bypassing macOS Tahoe's 
# local network restrictions for launchd processes
exec /usr/bin/open -W -a "$APP_PATH"
EOF
chmod +x "$LAUNCHER_SCRIPT"

echo "Created launcher script: $LAUNCHER_SCRIPT"

# Create the launchd plist file
# We run the launcher script which then uses `open -W -a` to launch the app
# The app binary reads environment from the process environment
cat > "$PLIST_PATH" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$PLIST_NAME</string>
    
    <key>ProgramArguments</key>
    <array>
        <string>$LAUNCHER_SCRIPT</string>
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
</dict>
</plist>
EOF

echo "Created launchd plist: $PLIST_PATH"

# Load the service
launchctl load "$PLIST_PATH"
echo "Service loaded: $PLIST_NAME"

# Check status
sleep 3
if launchctl list | grep -q "$PLIST_NAME"; then
    echo "Service is running"
    # Health check
    source "$SCRIPT_DIR/.env"
    PORT="${SERVER_PORT:-5001}"
    sleep 2  # Give the app a moment to start
    if curl -s "http://127.0.0.1:$PORT/health" | grep -q "OK"; then
        echo "Health check passed: http://127.0.0.1:$PORT"
    else
        echo "Warning: Health check failed (app may still be starting)"
        echo "Check logs: $SCRIPT_DIR/chunked-uploader.stderr.log"
    fi
else
    echo "Warning: Service may not have started correctly"
    echo "Check logs: $SCRIPT_DIR/chunked-uploader.stderr.log"
fi
