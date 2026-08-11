#!/bin/bash
# Builds fntype in release mode and installs /Applications/FnType.app
set -euo pipefail
export PATH="/opt/homebrew/bin:$PATH"
cd "$(dirname "$0")/.."

cargo build --release

APP="/Applications/FnType.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>FnType</string>
    <key>CFBundleIdentifier</key><string>com.curiosityos.fntype</string>
    <key>CFBundleName</key><string>FnType</string>
    <key>CFBundleDisplayName</key><string>FnType</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>FnType.icns</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>LSUIElement</key><true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>FnType records your voice while you hold the Fn key so it can be transcribed.</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

cp target/release/fntype "$APP/Contents/MacOS/FnType"
cp assets/FnType.icns "$APP/Contents/Resources/FnType.icns"

SIGNING_IDENTITY="${FNTYPE_SIGNING_IDENTITY:-FnType Local Development}"
if security find-identity -p codesigning | grep -Fq "\"$SIGNING_IDENTITY\""; then
    codesign --force --deep --timestamp=none --sign "$SIGNING_IDENTITY" "$APP"
    SIGNING_LABEL="$SIGNING_IDENTITY"
else
    codesign --force --deep --sign - "$APP"
    SIGNING_LABEL="ad-hoc; permissions may need reapproval after rebuilding"
fi
xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true
echo "Installed: $APP ($SIGNING_LABEL)"
