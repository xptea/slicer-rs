#!/usr/bin/env bash
# Reject stale export bundles before packaging a platform-specific app.
set -euo pipefail
ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
source "$ROOT_DIR/packaging/ffmpeg.lock"
BUNDLE=${1:?Usage: scripts/verify-export-bundle.sh DIR}
FFMPEG="$BUNDLE/bin/ffmpeg"
[[ -x "$FFMPEG" ]] || FFMPEG="$BUNDLE/bin/ffmpeg.exe"
if ! "$FFMPEG" -hide_banner -encoders 2>/dev/null | awk '$2 == "libopenh264" { found=1 } END { exit !found }'; then
    printf 'error: export bundle lacks unified OpenH264 encoding; rebuild with scripts/build-ffmpeg.sh for this target\n' >&2
    exit 1
fi
ARCHIVE="$BUNDLE/source/$OPENH264_SOURCE_ARCHIVE"
[[ -s "$ARCHIVE" && -s "$BUNDLE/COPYING.OPENH264" ]] || { printf 'error: OpenH264 source or license missing from export bundle\n' >&2; exit 1; }
if command -v sha256sum >/dev/null; then
    ACTUAL=$(sha256sum "$ARCHIVE" | awk '{print $1}')
else
    ACTUAL=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
fi
[[ "$ACTUAL" == "$OPENH264_SOURCE_SHA256" ]] || { printf 'error: OpenH264 source hash mismatch\n' >&2; exit 1; }
