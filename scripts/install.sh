#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
"$ROOT/scripts/bundle.sh"

if [[ "${1:-}" != "--no-login-item" ]]; then
    if ! osascript <<'APPLESCRIPT'
tell application "System Events"
    if exists login item "FnType" then delete login item "FnType"
    make login item at end with properties {name:"FnType", path:"/Applications/FnType.app", hidden:false}
end tell
APPLESCRIPT
    then
        echo "FnType installed, but the login item could not be added." >&2
        echo "Add /Applications/FnType.app in System Settings > General > Login Items." >&2
    fi
fi

open -n /Applications/FnType.app
echo "Launched FnType. Approve Microphone, Input Monitoring, and Accessibility when prompted."
