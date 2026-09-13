#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

test -f "$root/.gitmodules"
test -d "$root/viewer/ui/novnc/core"
grep -q 'read_only: true' "$root/compose.yaml"
grep -q 'restart: "no"' "$root/compose.yaml"
grep -q 'driver: none' "$root/compose.yaml"
grep -q 'seccomp=unconfined' "$root/compose.yaml"
if grep -qE '^ *ports:' "$root/compose.yaml"; then echo 'published port found' >&2; exit 1; fi
grep -q 'SESSION_RUNTIME_DIR' "$root/compose.yaml"
grep -q '/home/desktop:uid=${SESSION_UID' "$root/compose.yaml"
grep -q -- '-AcceptCutText=0 -SendCutText=0' "$root/images/desktop/ephemeral-session"
grep -q -- '--disable-gpu' "$root/images/desktop/ephemeral-session"
if grep -q -- '--no-sandbox' "$root/images/desktop/ephemeral-session"; then echo 'unsafe Chromium switch found' >&2; exit 1; fi
grep -q 'tauri_runtime_wry::Wry::default()' "$root/viewer/src-tauri/src/main.rs"
grep -q '\.incognito(true)' "$root/viewer/src-tauri/src/main.rs"
grep -q 'wsProtocols' "$root/viewer/ui/main.js"
if grep -qE 'clipboardPasteFrom[[:space:]]*\(|(location|window\.open)[^\n]*vnc\.html' "$root/viewer/ui/main.js"; then echo 'forbidden noVNC UI capability found' >&2; exit 1; fi
grep -q 'VIEWER_ORIGIN' "$root/viewer/src-tauri/src/session.rs"
grep -q '127.0.0.1:0' "$root/viewer/src-tauri/src/session.rs"
grep -q 'ensure_tmpfs' "$root/viewer/src-tauri/src/session.rs"
grep -q 'create_new(true)' "$root/viewer/src-tauri/src/session.rs"
bash -n "$root/scripts/preflight" "$root/images/desktop/ephemeral-session" "$root/scripts/build" "$root/viewer/launch"
SESSION_RUNTIME_DIR=/tmp SESSION_UID=1000 SESSION_GID=1000 docker compose -f "$root/compose.yaml" config >/dev/null
echo 'validation behavior tests: PASS'
