#!/usr/bin/env bash
# Assemble a macOS Slicer.app with the matching private FFmpeg tool pair.
#
# A bundle is accepted only when it contains the pinned upstream source
# archive and provenance files. This script never copies ffmpeg from PATH.
set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
LOCK_FILE="$ROOT_DIR/packaging/ffmpeg.lock"
# shellcheck disable=SC1090
source "$LOCK_FILE"

ARCH=${SLICER_MACOS_ARCH:-$(uname -m)}
case "$ARCH" in
    x86_64|amd64) ARCH=x86_64; TARGET=macos-x86_64 ;;
    arm64|aarch64) ARCH=aarch64; TARGET=macos-aarch64 ;;
    *)
        printf 'error: unsupported macOS architecture: %s\n' "$ARCH" >&2
        exit 2
        ;;
esac

BINARY_PATH=${SLICER_BINARY:-}
FFMPEG_BUNDLE=${SLICER_FFMPEG_BUNDLE:-$ROOT_DIR/build/ffmpeg/$TARGET}
RUST_NOTICES=${SLICER_RUST_NOTICES:-$ROOT_DIR/build/rust-notices}
DIST_ROOT=${SLICER_DIST_ROOT:-$ROOT_DIR/dist}
PACKAGE_NAME=${SLICER_PACKAGE_NAME:-slicer-macos-$ARCH}
JOBS=${SLICER_FFMPEG_JOBS:-}

usage() {
    cat <<'USAGE'
Usage: scripts/package-macos.sh [options]

Options:
  --binary FILE             Slicer executable (defaults to release, then debug)
  --ffmpeg-bundle DIR       build-ffmpeg.sh output directory
  --rust-notices DIR        collected Rust dependency notices
  --dist DIR                directory for the app and archive
  --name NAME               package directory/archive basename
  --jobs N                  make jobs when FFmpeg must be built
  -h, --help                Show this help

The output contains NAME/Slicer.app. Its executable is in Contents/MacOS;
FFmpeg and ffprobe are in Contents/lib/slicer/bin so the application can
resolve them through its executable-relative ../lib/slicer/bin layout.
USAGE
}

while (($# > 0)); do
    case "$1" in
        --binary)
            (($# >= 2)) || { printf 'error: --binary needs a value\n' >&2; exit 2; }
            BINARY_PATH=$2
            shift 2
            ;;
        --ffmpeg-bundle)
            (($# >= 2)) || { printf 'error: --ffmpeg-bundle needs a value\n' >&2; exit 2; }
            FFMPEG_BUNDLE=$2
            shift 2
            ;;
        --rust-notices)
            (($# >= 2)) || { printf 'error: --rust-notices needs a value\n' >&2; exit 2; }
            RUST_NOTICES=$2
            shift 2
            ;;
        --dist)
            (($# >= 2)) || { printf 'error: --dist needs a value\n' >&2; exit 2; }
            DIST_ROOT=$2
            shift 2
            ;;
        --name)
            (($# >= 2)) || { printf 'error: --name needs a value\n' >&2; exit 2; }
            PACKAGE_NAME=$2
            shift 2
            ;;
        --jobs)
            (($# >= 2)) || { printf 'error: --jobs needs a value\n' >&2; exit 2; }
            JOBS=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            printf 'error: unknown option: %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ -z "$BINARY_PATH" ]]; then
    if [[ -x "$ROOT_DIR/target/release/slicer" ]]; then
        BINARY_PATH="$ROOT_DIR/target/release/slicer"
    elif [[ -x "$ROOT_DIR/target/debug/slicer" ]]; then
        printf 'warning: release binary is absent; packaging target/debug/slicer\n' >&2
        BINARY_PATH="$ROOT_DIR/target/debug/slicer"
    else
        printf 'error: no Slicer executable; pass --binary FILE\n' >&2
        exit 1
    fi
fi
[[ -f "$BINARY_PATH" && -x "$BINARY_PATH" ]] || {
    printf 'error: Slicer binary is not executable: %s\n' "$BINARY_PATH" >&2
    exit 1
}

if [[ ! -x "$FFMPEG_BUNDLE/bin/ffmpeg" || ! -x "$FFMPEG_BUNDLE/bin/ffprobe" ]]; then
    printf 'FFmpeg bundle is missing; building the pinned source profile\n'
    BUILD_ARGS=(--target "$TARGET" --output "$FFMPEG_BUNDLE")
    [[ -n "$JOBS" ]] && BUILD_ARGS+=(--jobs "$JOBS")
    "$ROOT_DIR/scripts/build-ffmpeg.sh" "${BUILD_ARGS[@]}"
fi

sha256() {
    local file=$1
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 -- "$file" | awk '{print $1}'
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum -- "$file" | awk '{print $1}'
    else
        printf 'error: shasum or sha256sum is required\n' >&2
        return 1
    fi
}

verify_hash() {
    local file=$1 expected=$2 actual
    actual=$(sha256 "$file")
    [[ "$actual" == "$expected" ]] || {
        printf 'error: SHA-256 mismatch for %s\n  expected %s\n  actual   %s\n' \
            "$file" "$expected" "$actual" >&2
        return 1
    }
}

for program in ffmpeg ffprobe; do
    path="$FFMPEG_BUNDLE/bin/$program"
    [[ -f "$path" && -x "$path" ]] || {
        printf 'error: bundle lacks executable %s: %s\n' "$program" "$path" >&2
        exit 1
    }
done
SOURCE_ARCHIVE="$FFMPEG_BUNDLE/source/$FFMPEG_SOURCE_ARCHIVE"
[[ -f "$SOURCE_ARCHIVE" ]] || {
    printf 'error: FFmpeg source archive is required in the bundle: %s\n' "$SOURCE_ARCHIVE" >&2
    exit 1
}
verify_hash "$SOURCE_ARCHIVE" "$FFMPEG_SOURCE_SHA256"
for material in \
    "$FFMPEG_BUNDLE/COPYING.LGPLv2.1" \
    "$FFMPEG_BUNDLE/FFMPEG-NOTICE.txt" \
    "$FFMPEG_BUNDLE/source/PROVENANCE.txt"; do
    [[ -f "$material" ]] || {
        printf 'error: required FFmpeg source material is missing: %s\n' "$material" >&2
        exit 1
    }
done
[[ -d "$RUST_NOTICES" ]] || {
    printf 'error: Rust dependency notices are missing: %s\n' "$RUST_NOTICES" >&2
    printf 'Run tools/collect-rust-notices.py before packaging.\n' >&2
    exit 1
}

mkdir -p -- "$DIST_ROOT"
PACKAGE_DIR="$DIST_ROOT/$PACKAGE_NAME"
ARCHIVE_PATH="$DIST_ROOT/$PACKAGE_NAME.tar.gz"
if [[ -e "$PACKAGE_DIR" || -e "$ARCHIVE_PATH" ]]; then
    printf 'error: package output already exists; choose another --dist/--name: %s\n' \
        "$PACKAGE_DIR" >&2
    exit 1
fi

APP_DIR="$PACKAGE_DIR/Slicer.app"
mkdir -p -- "$APP_DIR/Contents/MacOS" \
    "$APP_DIR/Contents/lib/slicer/bin" \
    "$APP_DIR/Contents/Resources/slicer/ffmpeg-source" \
    "$APP_DIR/Contents/Resources/slicer/rust-notices"
install -m 755 "$BINARY_PATH" "$APP_DIR/Contents/MacOS/slicer"
install -m 755 "$FFMPEG_BUNDLE/bin/ffmpeg" "$APP_DIR/Contents/lib/slicer/bin/ffmpeg"
install -m 755 "$FFMPEG_BUNDLE/bin/ffprobe" "$APP_DIR/Contents/lib/slicer/bin/ffprobe"
cp -R "$FFMPEG_BUNDLE/source/." "$APP_DIR/Contents/Resources/slicer/ffmpeg-source/"
install -m 644 "$FFMPEG_BUNDLE/COPYING.LGPLv2.1" \
    "$APP_DIR/Contents/Resources/slicer/COPYING.LGPLv2.1"
install -m 644 "$FFMPEG_BUNDLE/FFMPEG-NOTICE.txt" \
    "$APP_DIR/Contents/Resources/slicer/FFMPEG-NOTICE.txt"
if [[ -f "$FFMPEG_BUNDLE/ZLIB-NOTICE.txt" ]]; then
    install -m 644 "$FFMPEG_BUNDLE/ZLIB-NOTICE.txt" \
        "$APP_DIR/Contents/Resources/slicer/ZLIB-NOTICE.txt"
fi
cp -R "$RUST_NOTICES/." "$APP_DIR/Contents/Resources/slicer/rust-notices/"
install -m 644 "$ROOT_DIR/docs/packaging.md" \
    "$APP_DIR/Contents/Resources/slicer/packaging.md"

cat >"$APP_DIR/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDisplayName</key><string>Slicer</string>
  <key>CFBundleExecutable</key><string>slicer</string>
  <key>CFBundleIdentifier</key><string>org.slicer.app</string>
  <key>CFBundleName</key><string>Slicer</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
</dict>
</plist>
PLIST

{
    printf 'Slicer macOS package manifest\n'
    printf 'Package:           %s\n' "$PACKAGE_NAME"
    printf 'FFmpeg version:    %s\n' "$FFMPEG_VERSION"
    printf 'FFmpeg source SHA: %s\n' "$FFMPEG_SOURCE_SHA256"
    printf '\nFiles (SHA-256):\n'
    while IFS= read -r path; do
        printf '%s  %s\n' "$(sha256 "$PACKAGE_DIR/$path")" "$path"
    done < <(cd "$PACKAGE_DIR" && find . -type f ! -name SHA256SUMS -print | sed 's#^./##' | LC_ALL=C sort)
} >"$PACKAGE_DIR/SHA256SUMS"

tar -C "$DIST_ROOT" -czf "$ARCHIVE_PATH" "$PACKAGE_NAME"
printf 'macOS package ready:\n  app:     %s\n  archive: %s\n' "$APP_DIR" "$ARCHIVE_PATH"
