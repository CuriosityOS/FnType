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

# Quit any running copy first. `open -n` used to leave a stale binary alive in
# memory after reinstall, so the old (buggy) process kept handling Fn.
osascript -e 'tell application "FnType" to quit' >/dev/null 2>&1 || true
pkill -x FnType >/dev/null 2>&1 || true
# Wait until the old process is gone so we don't race the new launch.
for _ in 1 2 3 4 5 6 7 8 9 10; do
    pgrep -x FnType >/dev/null 2>&1 || break
    sleep 0.2
done
if pgrep -x FnType >/dev/null 2>&1; then
    echo "FnType is still running; force-quitting…" >&2
    pkill -9 -x FnType >/dev/null 2>&1 || true
    sleep 0.3
fi

open /Applications/FnType.app
echo "Launched FnType. Approve Microphone, Input Monitoring, and Accessibility when prompted."
