#!/usr/bin/env bash
# Verify the app's installed layout and exercise exports without development overrides.
set -euo pipefail
ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
APP=${1:?Usage: scripts/smoke-macos-bundle.sh /path/Slicer.app}
APP=$(cd "$APP" && pwd)
unset SLICER_FFMPEG_DIR SLICER_MPV_LIBRARY
BINARY="$APP/Contents/MacOS/slicer"
codesign --verify --deep --strict "$APP"
python3 "$ROOT_DIR/scripts/bundle-playback-macos.py" --check "$APP/Contents/lib/slicer/playback"
plutil -lint "$APP/Contents/Info.plist"
[[ -s "$APP/Contents/Resources/Slicer.icns" ]] || { printf 'error: missing app icon\n' >&2; exit 1; }
WORK=$(mktemp -d)
trap 'rm -rf -- "$WORK"' EXIT
"$BINARY" binaries
"$APP/Contents/lib/slicer/bin/ffmpeg" -v error -f lavfi -i 'testsrc2=size=320x180:rate=30:duration=3' -f lavfi -i 'sine=frequency=440:duration=3' -c:v mpeg4 -c:a aac "$WORK/日本 input.mp4"
"$BINARY" inspect "$WORK/日本 input.mp4"
"$BINARY" preview "$WORK/日本 input.mp4" 1 "$WORK/frame.png"
"$BINARY" export "$WORK/日本 input.mp4" "$WORK/cut.mp4" 0.5 2 exact
"$BINARY" inspect "$WORK/cut.mp4"
"$APP/Contents/lib/slicer/bin/ffprobe" -v error -show_entries stream=codec_type,codec_name -of json "$WORK/cut.mp4" > "$WORK/codecs.json"
python3 - "$WORK/codecs.json" <<'PY'
import json, sys
streams = json.load(open(sys.argv[1]))["streams"]
assert any(s["codec_type"] == "video" and s["codec_name"] == "h264" for s in streams)
assert any(s["codec_type"] == "audio" and s["codec_name"] == "aac" for s in streams)
PY
[[ -s "$WORK/frame.png" && -s "$WORK/cut.mp4" ]]
printf 'macOS bundle smoke checks passed\n'
