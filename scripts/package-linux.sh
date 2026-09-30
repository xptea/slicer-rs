#!/usr/bin/env bash
# Assemble a Linux Slicer directory and tarball with private FFmpeg and mpv
# runtimes.
#
# This script never calls an ffmpeg found on PATH. It either consumes the
# verified output of build-ffmpeg.sh or builds that output from the pinned
# source archive first.
set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
LOCK_FILE="$ROOT_DIR/packaging/ffmpeg.lock"
# shellcheck disable=SC1090
source "$LOCK_FILE"

ARCH=$(uname -m)
case "$ARCH" in
    x86_64|amd64) ARCH=x86_64; TARGET=linux-x86_64 ;;
    aarch64|arm64) ARCH=arm64; TARGET=linux-aarch64 ;;
    *)
        printf 'error: unsupported Linux architecture: %s\n' "$ARCH" >&2
        exit 2
        ;;
esac

BINARY_PATH=${SLICER_BINARY:-}
FFMPEG_BUNDLE=${SLICER_FFMPEG_BUNDLE:-$ROOT_DIR/build/ffmpeg/$TARGET}
PLAYBACK_BUNDLE=${SLICER_PLAYBACK_BUNDLE:-$ROOT_DIR/build/playback/$TARGET}
PLAYBACK_SOURCE_CACHE=${SLICER_PLAYBACK_SOURCE_CACHE:-$ROOT_DIR/packaging/playback-source}
ALLOW_MISSING_PLAYBACK_SOURCES=${SLICER_PLAYBACK_ALLOW_MISSING_SOURCES:-0}
RUST_NOTICES=${SLICER_RUST_NOTICES:-$ROOT_DIR/build/rust-notices}
DIST_ROOT=${SLICER_DIST_ROOT:-$ROOT_DIR/dist}
PACKAGE_NAME=${SLICER_PACKAGE_NAME:-slicer-linux-$ARCH}
JOBS=${SLICER_FFMPEG_JOBS:-}

usage() {
    cat <<'USAGE'
Usage: scripts/package-linux.sh [options]

Options:
  --deb                     build an installable Debian package instead
  --binary FILE             Slicer executable (defaults to release, then debug)
  --ffmpeg-bundle DIR       build-ffmpeg.sh output directory
  --playback-bundle DIR     bundle-playback-linux.sh output directory
  --playback-source-cache DIR
                            exact libmpv dependency source archives
  --allow-missing-playback-sources
                            development-only; do not use for releases
  --rust-notices DIR        collected Rust dependency notices
  --dist DIR                directory for the package and tarball
  --name NAME               package directory/archive basename
  --jobs N                  make jobs when FFmpeg must be built
  -h, --help                Show this help

The package contains bin/slicer, lib/slicer/bin/{ffmpeg,ffprobe}, the pinned
FFmpeg source archive/signature, the libmpv playback runtime and its private
non-driver dependencies, an application source snapshot, and complete
license/source notices. Both source profiles are mandatory; a prebuilt
runtime without its source materials is rejected.
USAGE
}

# Keep the portable directory/tarball command backwards compatible while
# making the Debian production path discoverable from the existing entrypoint.
for argument in "$@"; do
    if [[ "$argument" == "--deb" ]]; then
        forwarded=()
        for candidate in "$@"; do
            [[ "$candidate" == "--deb" ]] || forwarded+=("$candidate")
        done
        exec "$SCRIPT_DIR/package-linux-deb.sh" "${forwarded[@]}"
    fi
done

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
        --playback-bundle)
            (($# >= 2)) || { printf 'error: --playback-bundle needs a value\n' >&2; exit 2; }
            PLAYBACK_BUNDLE=$2
            shift 2
            ;;
        --playback-source-cache)
            (($# >= 2)) || { printf 'error: --playback-source-cache needs a value\n' >&2; exit 2; }
            PLAYBACK_SOURCE_CACHE=$2
            shift 2
            ;;
        --allow-missing-playback-sources)
            ALLOW_MISSING_PLAYBACK_SOURCES=1
            shift
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

sha256() {
    local file=$1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -- "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 -- "$file" | awk '{print $1}'
    else
        printf 'error: sha256sum or shasum is required\n' >&2
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

if [[ ! -e "$PLAYBACK_BUNDLE/libmpv.so.2" ]]; then
    printf 'libmpv playback bundle is missing; assembling the native Linux runtime\n'
    PLAYBACK_ARGS=(--output "$PLAYBACK_BUNDLE" --source-cache "$PLAYBACK_SOURCE_CACHE")
    [[ "$ALLOW_MISSING_PLAYBACK_SOURCES" == 1 ]] &&
        PLAYBACK_ARGS+=(--allow-missing-sources)
    "$ROOT_DIR/scripts/bundle-playback-linux.sh" "${PLAYBACK_ARGS[@]}"
fi
[[ -e "$PLAYBACK_BUNDLE/libmpv.so.2" ]] || {
    printf 'error: playback bundle lacks libmpv.so.2: %s\n' "$PLAYBACK_BUNDLE" >&2
    printf 'The package is not release-ready without the native playback runtime.\n' >&2
    exit 1
}
for playback_material in PLAYBACK-NOTICE.txt SOURCE-MANIFEST.txt PROVENANCE.txt; do
    [[ -f "$PLAYBACK_BUNDLE/$playback_material" ]] || {
        printf 'error: playback bundle material is missing: %s/%s\n' \
            "$PLAYBACK_BUNDLE" "$playback_material" >&2
        exit 1
    }
done
if [[ "$ALLOW_MISSING_PLAYBACK_SOURCES" != 1 ]] &&
    grep -Eq '(^|[|[:space:]])MISSING([|[:space:]]|$)' "$PLAYBACK_BUNDLE/SOURCE-MANIFEST.txt"; then
    printf 'error: playback source manifest contains missing material; refusing release package\n' >&2
    exit 1
fi
"$ROOT_DIR/scripts/bundle-playback-linux.sh" --check "$PLAYBACK_BUNDLE"

if [[ ! -x "$FFMPEG_BUNDLE/bin/ffmpeg" || ! -x "$FFMPEG_BUNDLE/bin/ffprobe" ]] ||
    ! "$FFMPEG_BUNDLE/bin/ffmpeg" -hide_banner -encoders 2>/dev/null | awk '$2 == "libopenh264" { found=1 } END { exit !found }'; then
    printf 'FFmpeg bundle is missing or stale; building the pinned source profile\n'
    BUILD_ARGS=(--target "$TARGET" --output "$FFMPEG_BUNDLE")
    [[ -n "$JOBS" ]] && BUILD_ARGS+=(--jobs "$JOBS")
    "$ROOT_DIR/scripts/build-ffmpeg.sh" "${BUILD_ARGS[@]}"
fi

for program in ffmpeg ffprobe; do
    path="$FFMPEG_BUNDLE/bin/$program"
    [[ -f "$path" && -x "$path" ]] || {
        printf 'error: bundle lacks executable %s: %s\n' "$program" "$path" >&2
        exit 1
    }
done

"$ROOT_DIR/scripts/verify-export-bundle.sh" "$FFMPEG_BUNDLE"

SOURCE_ARCHIVE="$FFMPEG_BUNDLE/source/$FFMPEG_SOURCE_ARCHIVE"
[[ -f "$SOURCE_ARCHIVE" ]] || {
    printf 'error: FFmpeg source archive is required in the bundle: %s\n' "$SOURCE_ARCHIVE" >&2
    exit 1
}
verify_hash "$SOURCE_ARCHIVE" "$FFMPEG_SOURCE_SHA256"
[[ -f "$FFMPEG_BUNDLE/FFMPEG-NOTICE.txt" ]] || {
    printf 'error: FFmpeg notice is missing: %s\n' "$FFMPEG_BUNDLE/FFMPEG-NOTICE.txt" >&2
    exit 1
}
[[ -f "$FFMPEG_BUNDLE/source/PROVENANCE.txt" ]] || {
    printf 'error: FFmpeg provenance is missing: %s\n' "$FFMPEG_BUNDLE/source/PROVENANCE.txt" >&2
    exit 1
}
[[ -f "$FFMPEG_BUNDLE/COPYING.LGPLv2.1" ]] || {
    printf 'error: extracted FFmpeg license is missing: %s\n' \
        "$FFMPEG_BUNDLE/COPYING.LGPLv2.1" >&2
    exit 1
}
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

mkdir -p -- "$PACKAGE_DIR/bin" \
    "$PACKAGE_DIR/lib/slicer/bin" \
    "$PACKAGE_DIR/lib/slicer/playback" \
    "$PACKAGE_DIR/share/slicer/ffmpeg-source" \
    "$PACKAGE_DIR/share/slicer/playback-source" \
    "$PACKAGE_DIR/share/slicer/playback-notices" \
    "$PACKAGE_DIR/share/slicer/slicer-source" \
    "$PACKAGE_DIR/share/slicer" \
    "$PACKAGE_DIR/share/slicer/rust-notices" \
    "$PACKAGE_DIR/share/applications" \
    "$PACKAGE_DIR/share/icons/hicolor/256x256/apps"

copy_application_sources() {
    local source relative destination
    # The installed source snapshot keeps the Slicer MIT notice and the exact
    # source used to build the executable available next to the binary. Build
    # products, package outputs, VCS metadata, and user files are excluded.
    while IFS= read -r -d '' source; do
        relative=${source#"$ROOT_DIR/"}
        destination="$PACKAGE_DIR/share/slicer/slicer-source/$relative"
        mkdir -p -- "$(dirname -- "$destination")"
        cp -a -- "$source" "$destination"
    done < <(find "$ROOT_DIR" -xdev \
        \( -path "$ROOT_DIR/.git" -o -path "$ROOT_DIR/.agents" -o -path "$ROOT_DIR/.codex" \
            -o -path "$ROOT_DIR/build" -o -path "$ROOT_DIR/dist" -o -path "$ROOT_DIR/target" \
            -o -name userfiles -o -path "$ROOT_DIR/packaging/playback-source" \) -prune \
        -o -type f -print0 | sort -z)
}

install -m 755 "$BINARY_PATH" "$PACKAGE_DIR/bin/slicer"
install -m 644 "$ROOT_DIR/resources/slicer.desktop" \
    "$PACKAGE_DIR/share/applications/slicer.desktop"
install -m 644 "$ROOT_DIR/resources/icons/hicolor/256x256/apps/slicer.png" \
    "$PACKAGE_DIR/share/icons/hicolor/256x256/apps/slicer.png"
install -m 644 "$ROOT_DIR/LICENSE" "$PACKAGE_DIR/share/slicer/LICENSE"
install -m 755 "$FFMPEG_BUNDLE/bin/ffmpeg" "$PACKAGE_DIR/lib/slicer/bin/ffmpeg"
install -m 755 "$FFMPEG_BUNDLE/bin/ffprobe" "$PACKAGE_DIR/lib/slicer/bin/ffprobe"
cp -a "$FFMPEG_BUNDLE/source/." "$PACKAGE_DIR/share/slicer/ffmpeg-source/"
while IFS= read -r -d '' path; do
    cp -a -- "$path" "$PACKAGE_DIR/lib/slicer/playback/"
done < <(find "$PLAYBACK_BUNDLE" -maxdepth 1 \( -type f -o -type l \) -name 'lib*.so*' -print0 | sort -z)
cp -a "$PLAYBACK_BUNDLE/source/." "$PACKAGE_DIR/share/slicer/playback-source/"
cp -a "$PLAYBACK_BUNDLE/notices/." "$PACKAGE_DIR/share/slicer/playback-notices/"
install -m 644 "$PLAYBACK_BUNDLE/PLAYBACK-NOTICE.txt" \
    "$PACKAGE_DIR/share/slicer/PLAYBACK-NOTICE.txt"
install -m 644 "$PLAYBACK_BUNDLE/SOURCE-MANIFEST.txt" \
    "$PACKAGE_DIR/share/slicer/PLAYBACK-SOURCE-MANIFEST.txt"
install -m 644 "$PLAYBACK_BUNDLE/PROVENANCE.txt" \
    "$PACKAGE_DIR/share/slicer/PLAYBACK-PROVENANCE.txt"
install -m 644 "$FFMPEG_BUNDLE/COPYING.LGPLv2.1" \
    "$PACKAGE_DIR/share/slicer/COPYING.LGPLv2.1"
install -m 644 "$FFMPEG_BUNDLE/COPYING.OPENH264" "$PACKAGE_DIR/share/slicer/COPYING.OPENH264"
install -m 644 "$FFMPEG_BUNDLE/FFMPEG-NOTICE.txt" "$PACKAGE_DIR/share/slicer/FFMPEG-NOTICE.txt"
if [[ -f "$FFMPEG_BUNDLE/ZLIB-NOTICE.txt" ]]; then
    install -m 644 "$FFMPEG_BUNDLE/ZLIB-NOTICE.txt" "$PACKAGE_DIR/share/slicer/ZLIB-NOTICE.txt"
fi
cp -a "$RUST_NOTICES/." "$PACKAGE_DIR/share/slicer/rust-notices/"
copy_application_sources
install -m 644 "$ROOT_DIR/packaging/COMBINED-DISTRIBUTION-NOTICE.txt" \
    "$PACKAGE_DIR/share/slicer/COMBINED-DISTRIBUTION-NOTICE.txt"
install -m 644 "$ROOT_DIR/docs/packaging.md" "$PACKAGE_DIR/share/slicer/packaging.md"
if [[ -f "$ROOT_DIR/README.md" ]]; then
    install -m 644 "$ROOT_DIR/README.md" "$PACKAGE_DIR/README.md"
fi

{
    printf '# Slicer package manifest\n'
    printf '# Package:           %s\n' "$PACKAGE_NAME"
    printf '# FFmpeg version:    %s\n' "$FFMPEG_VERSION"
    printf '# FFmpeg source SHA: %s\n' "$FFMPEG_SOURCE_SHA256"
    printf '# Playback ABI:      libmpv.so.2\n'
    printf '# Playback sources:  share/slicer/PLAYBACK-SOURCE-MANIFEST.txt\n'
    printf '# Application source: share/slicer/slicer-source/\n'
    printf '\n# Files (SHA-256):\n'
    while IFS= read -r path; do
        printf '%s  %s\n' "$(sha256 "$PACKAGE_DIR/$path")" "$path"
    done < <(cd "$PACKAGE_DIR" && find . -type f ! -name SHA256SUMS -printf '%P\n' | LC_ALL=C sort)
} >"$PACKAGE_DIR/SHA256SUMS"

if tar --help 2>/dev/null | grep -Fq -- '--sort'; then
    tar --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
        -C "$DIST_ROOT" -czf "$ARCHIVE_PATH" "$PACKAGE_NAME"
else
    tar -C "$DIST_ROOT" -czf "$ARCHIVE_PATH" "$PACKAGE_NAME"
fi

printf 'Linux package ready:\n  directory: %s\n  archive:   %s\n' "$PACKAGE_DIR" "$ARCHIVE_PATH"
