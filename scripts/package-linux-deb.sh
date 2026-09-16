#!/usr/bin/env bash
# Build the release Linux distribution as an installable Debian package.
#
# The existing package-linux.sh layout remains useful as a portable, audited
# directory and tarball. This script assembles only the runtime payload needed
# by an installed application, without copying the portable bundle's source
# archives or build metadata.
set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)

ARCH=$(uname -m)
case "$ARCH" in
    x86_64|amd64) ARCH=x86_64; DEB_ARCH=amd64 ;;
    aarch64|arm64) ARCH=arm64; DEB_ARCH=arm64 ;;
    *)
        printf 'error: unsupported Linux architecture: %s\n' "$ARCH" >&2
        exit 2
        ;;
esac

DIST_ROOT=${SLICER_DIST_ROOT:-$ROOT_DIR/dist}
BINARY_PATH=${SLICER_BINARY:-}
FFMPEG_BUNDLE=${SLICER_FFMPEG_BUNDLE:-$ROOT_DIR/build/ffmpeg/linux-$ARCH}
if [[ -n "${SLICER_PLAYBACK_BUNDLE:-}" ]]; then
    PLAYBACK_BUNDLE=$SLICER_PLAYBACK_BUNDLE
elif [[ -e "$ROOT_DIR/build/playback-minimal/linux-$ARCH/libmpv.so.2" ]]; then
    # Prefer the source-built reduced runtime when this checkout has one.
    # The explicit environment override above remains available for a
    # source-complete or distro-provided closure.
    PLAYBACK_BUNDLE="$ROOT_DIR/build/playback-minimal/linux-$ARCH"
else
    PLAYBACK_BUNDLE="$ROOT_DIR/build/playback/linux-$ARCH"
fi
PLAYBACK_SOURCE_CACHE=${SLICER_PLAYBACK_SOURCE_CACHE:-$ROOT_DIR/packaging/playback-source}
RUST_NOTICES=${SLICER_RUST_NOTICES:-$ROOT_DIR/build/rust-notices}
ALLOW_MISSING_PLAYBACK_SOURCES=${SLICER_PLAYBACK_ALLOW_MISSING_SOURCES:-0}
JOBS=${SLICER_FFMPEG_JOBS:-}
DEB_NAME=${SLICER_DEB_NAME:-slicer}
DEB_VERSION=${SLICER_DEB_VERSION:-}
DEB_DEPENDS=${SLICER_DEB_DEPENDS:-}
KEEP_STAGING=${SLICER_DEB_KEEP_STAGING:-0}

if [[ -z "$DEB_VERSION" ]]; then
    DEB_VERSION=$(awk -F '"' '$1 ~ /^version[[:space:]]*=/ { print $2; exit }' \
        "$ROOT_DIR/Cargo.toml")
fi

usage() {
    cat <<'USAGE'
Usage: scripts/package-linux-deb.sh [options]

Build a lean release Slicer .deb. The script assembles only the Linux runtime
payload, then installs it below /usr/lib/slicer in the Debian archive and adds
/usr/bin/slicer plus the desktop icon and launcher.

Options:
  --binary FILE             release Slicer executable
  --ffmpeg-bundle DIR       build-ffmpeg.sh output directory
  --playback-bundle DIR     bundle-playback-linux.sh output directory
  --playback-source-cache DIR
                            exact libmpv dependency source archives
  --allow-missing-playback-sources
                            development-only; do not use for releases
  --rust-notices DIR        accepted for compatibility; not copied into .deb
  --dist DIR                output directory (defaults to dist)
  --name NAME               Debian package name (defaults to slicer)
  --version VERSION         Debian package version (defaults to Cargo.toml)
  --depends LIST            override generated shared-library dependencies
  --jobs N                  make jobs when FFmpeg must be built
  -h, --help                Show this help

The release binary is built with cargo build --release --locked when it is
not supplied and target/release/slicer is absent. When
build/playback-minimal/linux-<arch> exists, it is selected automatically;
pass --playback-bundle to choose another closure. Existing output is never
overwritten. Source archives remain available to the full portable packager;
they are intentionally not copied into this runtime installer. If a playback
bundle must be built from an installed libmpv, the strict source policy still
applies unless --allow-missing-playback-sources is explicitly supplied.
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
            DEB_NAME=$2
            shift 2
            ;;
        --version)
            (($# >= 2)) || { printf 'error: --version needs a value\n' >&2; exit 2; }
            DEB_VERSION=$2
            shift 2
            ;;
        --depends)
            (($# >= 2)) || { printf 'error: --depends needs a value\n' >&2; exit 2; }
            DEB_DEPENDS=$2
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

[[ "$DEB_NAME" =~ ^[a-z0-9][a-z0-9+.-]*$ ]] || {
    printf 'error: invalid Debian package name: %s\n' "$DEB_NAME" >&2
    exit 2
}
[[ "$DEB_VERSION" =~ ^[0-9][0-9A-Za-z.+:~_-]*$ ]] || {
    printf 'error: invalid Debian package version: %s\n' "$DEB_VERSION" >&2
    exit 2
}

if [[ -z "$BINARY_PATH" ]]; then
    BINARY_PATH="$ROOT_DIR/target/release/slicer"
    if [[ ! -x "$BINARY_PATH" ]]; then
        printf 'Release binary is absent; building target/release/slicer\n'
        (cd "$ROOT_DIR" && cargo build --release --locked)
    fi
fi
[[ -f "$BINARY_PATH" && -x "$BINARY_PATH" ]] || {
    printf 'error: release Slicer binary is not executable: %s\n' "$BINARY_PATH" >&2
    exit 1
}

command -v dpkg-deb >/dev/null 2>&1 || {
    printf 'error: dpkg-deb is required to build a Debian package\n' >&2
    exit 1
}

mkdir -p -- "$DIST_ROOT"
# Resolve this after creating it so paths used from inside dpkg-shlibdeps' cwd
# remain valid even when the caller supplied a relative --dist directory.
DIST_ROOT=$(cd -- "$DIST_ROOT" && pwd)
DEB_PATH="$DIST_ROOT/${DEB_NAME}_${DEB_VERSION}_${DEB_ARCH}.deb"
[[ ! -e "$DEB_PATH" ]] || {
    printf 'error: package output already exists; choose another --dist/--name/--version: %s\n' \
        "$DEB_PATH" >&2
    exit 1
}

STAGING_ROOT=$(mktemp -d "$DIST_ROOT/.${DEB_NAME}-deb.XXXXXX")
cleanup() {
    if [[ "$KEEP_STAGING" == 1 ]]; then
        printf 'Debian staging directory kept at: %s\n' "$STAGING_ROOT"
    else
        rm -rf -- "$STAGING_ROOT"
    fi
}
trap cleanup EXIT INT TERM

PAYLOAD_NAME="${DEB_NAME}-linux-${ARCH}-payload"
PAYLOAD_DIR="$STAGING_ROOT/$PAYLOAD_NAME"

# A Debian runtime package does not need the source-complete portable bundle.
# Validate or build the two media runtimes directly so source archives never
# enter the temporary staging tree in the first place.
if [[ ! -e "$PLAYBACK_BUNDLE/libmpv.so.2" ]]; then
    printf 'libmpv playback bundle is absent; assembling the native runtime\n'
    PLAYBACK_ARGS=(--output "$PLAYBACK_BUNDLE" --source-cache "$PLAYBACK_SOURCE_CACHE")
    [[ "$ALLOW_MISSING_PLAYBACK_SOURCES" == 1 ]] &&
        PLAYBACK_ARGS+=(--allow-missing-sources)
    "$ROOT_DIR/scripts/bundle-playback-linux.sh" "${PLAYBACK_ARGS[@]}"
fi
[[ -e "$PLAYBACK_BUNDLE/libmpv.so.2" ]] || {
    printf 'error: playback bundle lacks libmpv.so.2: %s\n' "$PLAYBACK_BUNDLE" >&2
    exit 1
}
"$ROOT_DIR/scripts/bundle-playback-linux.sh" --check "$PLAYBACK_BUNDLE"
[[ -f "$PLAYBACK_BUNDLE/PLAYBACK-NOTICE.txt" ]] || {
    printf 'error: playback notice is missing: %s\n' "$PLAYBACK_BUNDLE/PLAYBACK-NOTICE.txt" >&2
    exit 1
}

if [[ ! -x "$FFMPEG_BUNDLE/bin/ffmpeg" || ! -x "$FFMPEG_BUNDLE/bin/ffprobe" ]]; then
    printf 'FFmpeg bundle is absent; building the pinned source profile\n'
    BUILD_ARGS=(--target "linux-$ARCH" --output "$FFMPEG_BUNDLE")
    [[ -n "$JOBS" ]] && BUILD_ARGS+=(--jobs "$JOBS")
    "$ROOT_DIR/scripts/build-ffmpeg.sh" "${BUILD_ARGS[@]}"
fi
for program in ffmpeg ffprobe; do
    [[ -x "$FFMPEG_BUNDLE/bin/$program" ]] || {
        printf 'error: bundle lacks executable %s: %s\n' "$program" "$FFMPEG_BUNDLE/bin/$program" >&2
        exit 1
    }
done
for notice in COPYING.LGPLv2.1 FFMPEG-NOTICE.txt; do
    [[ -f "$FFMPEG_BUNDLE/$notice" ]] || {
        printf 'error: FFmpeg notice is missing: %s\n' "$FFMPEG_BUNDLE/$notice" >&2
        exit 1
    }
done
[[ -f "$ROOT_DIR/packaging/COMBINED-DISTRIBUTION-NOTICE.txt" ]] || {
    printf 'error: combined distribution notice is missing\n' >&2
    exit 1
}

mkdir -p -- \
    "$PAYLOAD_DIR/bin" \
    "$PAYLOAD_DIR/lib/slicer/bin" \
    "$PAYLOAD_DIR/lib/slicer/playback" \
    "$PAYLOAD_DIR/share/slicer" \
    "$PAYLOAD_DIR/share/slicer/playback-notices" \
    "$PAYLOAD_DIR/share/applications" \
    "$PAYLOAD_DIR/share/icons/hicolor/256x256/apps"
install -m 755 "$BINARY_PATH" "$PAYLOAD_DIR/bin/slicer"
install -m 755 "$FFMPEG_BUNDLE/bin/ffmpeg" "$PAYLOAD_DIR/lib/slicer/bin/ffmpeg"
install -m 755 "$FFMPEG_BUNDLE/bin/ffprobe" "$PAYLOAD_DIR/lib/slicer/bin/ffprobe"
install -m 644 "$ROOT_DIR/resources/slicer.desktop" \
    "$PAYLOAD_DIR/share/applications/slicer.desktop"
install -m 644 "$ROOT_DIR/resources/icons/hicolor/256x256/apps/slicer.png" \
    "$PAYLOAD_DIR/share/icons/hicolor/256x256/apps/slicer.png"
install -m 644 "$ROOT_DIR/LICENSE" "$PAYLOAD_DIR/share/slicer/LICENSE"
install -m 644 "$ROOT_DIR/packaging/COMBINED-DISTRIBUTION-NOTICE.txt" \
    "$PAYLOAD_DIR/share/slicer/COMBINED-DISTRIBUTION-NOTICE.txt"
install -m 644 "$FFMPEG_BUNDLE/COPYING.LGPLv2.1" "$PAYLOAD_DIR/share/slicer/COPYING.LGPLv2.1"
install -m 644 "$FFMPEG_BUNDLE/FFMPEG-NOTICE.txt" "$PAYLOAD_DIR/share/slicer/FFMPEG-NOTICE.txt"
if [[ -f "$FFMPEG_BUNDLE/ZLIB-NOTICE.txt" ]]; then
    install -m 644 "$FFMPEG_BUNDLE/ZLIB-NOTICE.txt" "$PAYLOAD_DIR/share/slicer/ZLIB-NOTICE.txt"
fi
install -m 644 "$PLAYBACK_BUNDLE/PLAYBACK-NOTICE.txt" \
    "$PAYLOAD_DIR/share/slicer/PLAYBACK-NOTICE.txt"
if [[ -d "$PLAYBACK_BUNDLE/notices" ]]; then
    cp -a -- "$PLAYBACK_BUNDLE/notices/." "$PAYLOAD_DIR/share/slicer/playback-notices/"
fi
while IFS= read -r -d '' path; do
    cp -a -- "$path" "$PAYLOAD_DIR/lib/slicer/playback/"
done < <(find "$PLAYBACK_BUNDLE" -maxdepth 1 \( -type f -o -type l \) -name 'lib*.so*' -print0 | sort -z)

DEB_ROOT="$STAGING_ROOT/debian-root"
mkdir -p -- \
    "$DEB_ROOT/DEBIAN" \
    "$DEB_ROOT/usr/bin" \
    "$DEB_ROOT/usr/lib/slicer" \
    "$DEB_ROOT/usr/share/applications" \
    "$DEB_ROOT/usr/share/icons/hicolor/256x256/apps" \
    "$DEB_ROOT/usr/share/doc/$DEB_NAME"

# The payload and Debian root are created under the same filesystem. Hard
# links avoid a second temporary gigabyte-scale copy; the fallback keeps the
# script usable when a custom --dist directory crosses filesystems.
if ! cp -al -- "$PAYLOAD_DIR/." "$DEB_ROOT/usr/lib/slicer/"; then
    rm -rf -- "$DEB_ROOT/usr/lib/slicer"
    mkdir -p -- "$DEB_ROOT/usr/lib/slicer"
    cp -a -- "$PAYLOAD_DIR/." "$DEB_ROOT/usr/lib/slicer/"
fi

# Desktop integration belongs in the system locations. The copy below is
# removed from the private payload so the installed package has one launcher
# and one icon.
install -m 644 "$PAYLOAD_DIR/share/applications/slicer.desktop" \
    "$DEB_ROOT/usr/share/applications/slicer.desktop"
sed -i 's|^Exec=.*|Exec=/usr/bin/slicer %F|' \
    "$DEB_ROOT/usr/share/applications/slicer.desktop"
install -m 644 "$PAYLOAD_DIR/share/icons/hicolor/256x256/apps/slicer.png" \
    "$DEB_ROOT/usr/share/icons/hicolor/256x256/apps/slicer.png"
rm -rf -- "$DEB_ROOT/usr/lib/slicer/share/applications" \
    "$DEB_ROOT/usr/lib/slicer/share/icons"
ln -s ../lib/slicer/bin/slicer "$DEB_ROOT/usr/bin/slicer"

# Keep the application license available through the standard Debian docs
# location while the runtime payload stays limited to executable media files.
install -m 644 "$PAYLOAD_DIR/share/slicer/LICENSE" \
    "$DEB_ROOT/usr/share/doc/$DEB_NAME/copyright"

# Generate dependencies from the actual ELF closure when dpkg-shlibdeps is
# available. A caller can provide --depends to pin a distro-specific list;
# the conservative fallback still records the libc requirement when building
# on a minimal packaging host.
if [[ -z "$DEB_DEPENDS" ]] && command -v dpkg-shlibdeps >/dev/null 2>&1; then
    # dpkg-shlibdeps treats binaries below a DEBIAN/ directory as already
    # installed package members and skips the private libmpv closure. Hide the
    # empty control directory while it scans, then recreate it for dpkg-deb.
    rm -rf -- "$DEB_ROOT/DEBIAN"
    mkdir -p -- "$DEB_ROOT/debian"
    cat >"$DEB_ROOT/debian/control" <<EOF
Source: $DEB_NAME
Section: video
Priority: optional
Maintainer: Slicer contributors <slicer@example.invalid>
Standards-Version: 4.6.0

Package: $DEB_NAME
Architecture: $DEB_ARCH
Description: Native video trimming toolbox
 Slicer video editor.
EOF
    SHLIB_LOG="$STAGING_ROOT/dpkg-shlibdeps.log"
    set +e
    SHLIB_OUTPUT=$(cd "$DEB_ROOT" && dpkg-shlibdeps -O \
        -e "$DEB_ROOT/usr/lib/slicer/bin/slicer" \
        -e "$DEB_ROOT/usr/lib/slicer/lib/slicer/bin/ffmpeg" \
        -e "$DEB_ROOT/usr/lib/slicer/lib/slicer/bin/ffprobe" \
        -e "$DEB_ROOT/usr/lib/slicer/lib/slicer/playback/libmpv.so.2" \
        2>"$SHLIB_LOG")
    SHLIB_STATUS=$?
    set -e
    if [[ "$SHLIB_STATUS" == 0 ]]; then
        DEB_DEPENDS=$(printf '%s\n' "$SHLIB_OUTPUT" |
            sed -n 's/^shlibs:Depends=//p' | tail -n 1)
    else
        printf 'warning: dpkg-shlibdeps could not resolve all dependencies; see %s\n' \
            "$SHLIB_LOG" >&2
    fi
    rm -rf -- "$DEB_ROOT/debian" "$DEB_ROOT/DEBIAN"
    mkdir -p -- "$DEB_ROOT/DEBIAN"
fi
DEB_DEPENDS=${DEB_DEPENDS:-libc6}

INSTALLED_SIZE=$(du -sk -- "$DEB_ROOT" | awk '{print $1}')
cat >"$DEB_ROOT/DEBIAN/control" <<EOF
Package: $DEB_NAME
Version: $DEB_VERSION
Section: video
Priority: optional
Architecture: $DEB_ARCH
Installed-Size: $INSTALLED_SIZE
Maintainer: Slicer contributors <slicer@example.invalid>
Depends: $DEB_DEPENDS
Description: Native video trimming toolbox
 Slicer is a native editor for trimming, cropping, and exporting video clips.
EOF

dpkg-deb --build --root-owner-group "$DEB_ROOT" "$DEB_PATH" >/dev/null
dpkg-deb --info "$DEB_PATH" >/dev/null

printf 'Debian package ready:\n  package: %s\n  size:    %s\n' \
    "$DEB_PATH" "$(du -h -- "$DEB_PATH" | awk '{print $1}')"
