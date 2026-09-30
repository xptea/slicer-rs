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
PLAYBACK_BUNDLE=${SLICER_PLAYBACK_BUNDLE:-$ROOT_DIR/build/playback/$TARGET}
SIGN_IDENTITY=${SLICER_SIGN_IDENTITY:--}
NOTARY_PROFILE=${SLICER_NOTARY_PROFILE:-}
MAKE_DMG=0
ALLOW_MISSING_PLAYBACK_SOURCES=0

usage() {
    cat <<'USAGE'
Usage: scripts/package-macos.sh [options]

Options:
  --binary FILE             Slicer executable (defaults to release, then debug)
  --ffmpeg-bundle DIR       build-ffmpeg.sh output directory
  --rust-notices DIR        collected Rust dependency notices
  --dist DIR                directory for the app and archive
  --name NAME               package directory/archive basename
  --playback-bundle DIR      relocated libmpv dylib closure
  --sign-identity ID         Developer ID Application identity (default: ad hoc)
  --notary-profile NAME      saved notarytool keychain profile; notarize and staple
  --dmg                     also create a drag-to-Applications disk image
  --allow-missing-playback-sources  development-only source-incomplete runtime
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
        --playback-bundle|--sign-identity|--notary-profile)
            (($# >= 2)) || { printf 'error: %s needs a value\n' "$1" >&2; exit 2; }
            case "$1" in
                --playback-bundle) PLAYBACK_BUNDLE=$2 ;;
                --sign-identity) SIGN_IDENTITY=$2 ;;
                --notary-profile) NOTARY_PROFILE=$2 ;;
            esac
            shift 2
            ;;
        --dmg) MAKE_DMG=1; shift ;;
        --allow-missing-playback-sources) ALLOW_MISSING_PLAYBACK_SOURCES=1; shift ;;
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

[[ $(uname -s) == Darwin ]] || { printf 'error: run this packager on macOS\n' >&2; exit 1; }
if [[ -n "$NOTARY_PROFILE" && "$SIGN_IDENTITY" == - ]]; then
    printf 'error: notarization requires --sign-identity with a Developer ID Application certificate\n' >&2
    exit 1
fi
MACH_ARCH=$ARCH
[[ "$ARCH" == aarch64 ]] && MACH_ARCH=arm64
python3 "$ROOT_DIR/scripts/bundle-playback-macos.py" --check "$PLAYBACK_BUNDLE" --arch "$MACH_ARCH"
for material in PLAYBACK-NOTICE.txt SOURCE-MANIFEST.txt PROVENANCE.json; do
    [[ -f "$PLAYBACK_BUNDLE/$material" ]] || { printf 'error: playback material missing: %s\n' "$material" >&2; exit 1; }
done
if [[ "$ALLOW_MISSING_PLAYBACK_SOURCES" != 1 ]] && grep -q '^MISSING' "$PLAYBACK_BUNDLE/SOURCE-MANIFEST.txt"; then
    printf 'error: playback sources missing; --allow-missing-playback-sources is development-only\n' >&2
    exit 1
fi

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

if ! "$FFMPEG_BUNDLE/bin/ffmpeg" -hide_banner -encoders 2>/dev/null | awk '$2 == "libopenh264" { found=1 } END { exit !found }'; then
    printf 'error: FFmpeg bundle lacks unified H.264 export; rebuild with scripts/build-ffmpeg.sh --target %s --output %s\n' "$TARGET" "$FFMPEG_BUNDLE" >&2
    exit 1
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
for path in "$BINARY_PATH" "$FFMPEG_BUNDLE/bin/ffmpeg" "$FFMPEG_BUNDLE/bin/ffprobe"; do
    [[ " $(lipo -archs "$path") " == *" $MACH_ARCH "* ]] || { printf 'error: wrong architecture: %s\n' "$path" >&2; exit 1; }
done
"$ROOT_DIR/scripts/verify-export-bundle.sh" "$FFMPEG_BUNDLE"

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
DMG_PATH="$DIST_ROOT/$PACKAGE_NAME.dmg"
if [[ -e "$PACKAGE_DIR" || -e "$ARCHIVE_PATH" || -e "$DMG_PATH" ]]; then
    printf 'error: package output already exists; choose another --dist/--name: %s\n' \
        "$PACKAGE_DIR" >&2
    exit 1
fi

APP_DIR="$PACKAGE_DIR/Slicer.app"
SOURCE_DIR="$PACKAGE_DIR/corresponding-source"
mkdir -p -- "$APP_DIR/Contents/MacOS" \
    "$APP_DIR/Contents/lib/slicer/bin" \
    "$APP_DIR/Contents/lib/slicer/playback" \
    "$SOURCE_DIR/ffmpeg" \
    "$APP_DIR/Contents/Resources/slicer/rust-notices"
install -m 755 "$BINARY_PATH" "$APP_DIR/Contents/MacOS/slicer"
install -m 755 "$FFMPEG_BUNDLE/bin/ffmpeg" "$APP_DIR/Contents/lib/slicer/bin/ffmpeg"
install -m 755 "$FFMPEG_BUNDLE/bin/ffprobe" "$APP_DIR/Contents/lib/slicer/bin/ffprobe"
cp -R "$FFMPEG_BUNDLE/source/." "$SOURCE_DIR/ffmpeg/"
install -m 644 "$FFMPEG_BUNDLE/COPYING.LGPLv2.1" \
    "$APP_DIR/Contents/Resources/slicer/COPYING.LGPLv2.1"
install -m 644 "$FFMPEG_BUNDLE/COPYING.OPENH264" "$APP_DIR/Contents/Resources/slicer/COPYING.OPENH264"
install -m 644 "$FFMPEG_BUNDLE/FFMPEG-NOTICE.txt" \
    "$APP_DIR/Contents/Resources/slicer/FFMPEG-NOTICE.txt"
if [[ -f "$FFMPEG_BUNDLE/ZLIB-NOTICE.txt" ]]; then
    install -m 644 "$FFMPEG_BUNDLE/ZLIB-NOTICE.txt" \
        "$APP_DIR/Contents/Resources/slicer/ZLIB-NOTICE.txt"
fi
cp -R "$PLAYBACK_BUNDLE/." "$APP_DIR/Contents/lib/slicer/playback/"
if [[ -d "$APP_DIR/Contents/lib/slicer/playback/source" ]]; then
    mv "$APP_DIR/Contents/lib/slicer/playback/source" "$SOURCE_DIR/playback"
fi
cp -R "$RUST_NOTICES/." "$APP_DIR/Contents/Resources/slicer/rust-notices/"
install -m 644 "$ROOT_DIR/docs/packaging.md" \
    "$APP_DIR/Contents/Resources/slicer/packaging.md"

# Build a complete native icon family so Finder, Dock, and the DMG use one icon.
ICON_WORK=$(mktemp -d)
trap 'rm -rf -- "$ICON_WORK"' EXIT
mkdir -p "$ICON_WORK/Slicer.iconset"
ICON_INPUT="$ROOT_DIR/resources/icons/hicolor/256x256/apps/slicer.png"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$ICON_INPUT" --out "$ICON_WORK/Slicer.iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$ICON_INPUT" --out "$ICON_WORK/Slicer.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICON_WORK/Slicer.iconset" -o "$APP_DIR/Contents/Resources/Slicer.icns"
python3 - "$ROOT_DIR/Cargo.toml" "$APP_DIR/Contents/Info.plist" "$PACKAGE_NAME" <<'PYINFO'
import pathlib, plistlib, re, subprocess, sys
version = re.search(r'^version\s*=\s*"([^"]+)"', pathlib.Path(sys.argv[1]).read_text(), re.M).group(1)
info = {
    'CFBundleDisplayName': 'Slicer', 'CFBundleExecutable': 'slicer',
    'CFBundleIdentifier': 'org.slicer.app', 'CFBundleName': 'Slicer',
    'CFBundlePackageType': 'APPL', 'CFBundleShortVersionString': version,
    'CFBundleVersion': version, 'CFBundleIconFile': 'Slicer.icns',
    # Derive the real minimum from every binary rather than promising an older
    # macOS that the supplied playback libraries cannot load on.
    'NSHighResolutionCapable': True,
    'CFBundleDocumentTypes': [{'CFBundleTypeName': 'Video', 'CFBundleTypeRole': 'Viewer',
        'LSHandlerRank': 'Alternate', 'LSItemContentTypes': ['public.movie', 'public.video']}],
}
contents = pathlib.Path(sys.argv[2]).parent
(contents / 'Resources/slicer/SOURCE-DOWNLOAD.txt').write_text(f'''Corresponding media source archives are distributed alongside Slicer.app:
https://github.com/xptea/slicer-rs/releases/download/v{version}/{sys.argv[3]}.tar.gz
The archive includes corresponding-source/ffmpeg and corresponding-source/playback.
Slicer application source and build recipes for this version:
https://github.com/xptea/slicer-rs/archive/refs/tags/v{version}.tar.gz
These sources are not needed to run the app. Keep the matching source archives
available when distributing the DMG or application. License notices remain in
the application bundle.
''')
binaries = [contents / 'MacOS/slicer', *list((contents / 'lib/slicer/bin').iterdir()),
            *list((contents / 'lib/slicer/playback').glob('*.dylib'))]
minimums = [(11, 0)]
for binary in binaries:
    load_commands = subprocess.check_output(['otool', '-l', str(binary)], text=True)
    for command in load_commands.split('Load command '):
        if 'LC_BUILD_VERSION' in command:
            match = re.search(r'minos\s+(\d+\.\d+(?:\.\d+)?)', command)
        elif 'LC_VERSION_MIN_MACOSX' in command:
            match = re.search(r'version\s+(\d+\.\d+(?:\.\d+)?)', command)
        else:
            continue
        if match:
            minimums.append(tuple(map(int, match.group(1).split('.'))))
info['LSMinimumSystemVersion'] = '.'.join(map(str, max(minimums)))
print('Bundle minimum macOS: ' + info['LSMinimumSystemVersion'])
pathlib.Path(sys.argv[2]).write_bytes(plistlib.dumps(info))
PYINFO

# Remove build-machine metadata before signing. A recipient's browser can add
# quarantine again; this does not replace Apple's notarization requirement.
xattr -cr "$APP_DIR"
# Sign inside out. Never rely on --deep to sign nested executable code.
SIGN_ARGS=(--force --sign "$SIGN_IDENTITY")
if [[ "$SIGN_IDENTITY" != - ]]; then SIGN_ARGS+=(--options runtime --timestamp); fi
while IFS= read -r -d '' library; do
    codesign "${SIGN_ARGS[@]}" "$library"
done < <(find "$APP_DIR/Contents/lib/slicer/playback" -maxdepth 1 -name '*.dylib' -type f -print0)
for executable in "$APP_DIR/Contents/lib/slicer/bin/ffmpeg" "$APP_DIR/Contents/lib/slicer/bin/ffprobe" "$APP_DIR/Contents/MacOS/slicer"; do
    codesign "${SIGN_ARGS[@]}" "$executable"
done
codesign "${SIGN_ARGS[@]}" "$APP_DIR"
codesign --verify --deep --strict "$APP_DIR"

if [[ -n "$NOTARY_PROFILE" ]]; then
    ditto -c -k --keepParent "$APP_DIR" "$ICON_WORK/notarize.zip"
    xcrun notarytool submit "$ICON_WORK/notarize.zip" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$APP_DIR"
    xcrun stapler validate "$APP_DIR"
    spctl --assess --type execute --verbose=2 "$APP_DIR"
fi

if [[ "$MAKE_DMG" == 1 ]]; then
    mkdir -p "$ICON_WORK/dmg"
    ditto "$APP_DIR" "$ICON_WORK/dmg/Slicer.app"
    ln -s /Applications "$ICON_WORK/dmg/Applications"
    cp "$ROOT_DIR/packaging/MACOS-INSTALL.txt" "$ICON_WORK/dmg/Read me first.txt"
    hdiutil create -volname Slicer -srcfolder "$ICON_WORK/dmg" -format UDZO "$DMG_PATH"
    if [[ "$SIGN_IDENTITY" != - ]]; then
        codesign --force --sign "$SIGN_IDENTITY" --timestamp "$DMG_PATH"
    fi
    if [[ -n "$NOTARY_PROFILE" ]]; then
        xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" --wait
        xcrun stapler staple "$DMG_PATH"
        xcrun stapler validate "$DMG_PATH"
        spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG_PATH"
    fi
fi

{
    printf 'Slicer macOS package manifest\n'
    printf 'Package:           %s\n' "$PACKAGE_NAME"
    printf 'Signing identity:  %s\n' "$SIGN_IDENTITY"
    printf 'Notarized:         %s\n' "${NOTARY_PROFILE:+yes}"
    printf 'FFmpeg version:    %s\n' "$FFMPEG_VERSION"
    printf 'FFmpeg source SHA: %s\n' "$FFMPEG_SOURCE_SHA256"
    printf '\nFiles (SHA-256):\n'
    while IFS= read -r path; do
        printf '%s  %s\n' "$(sha256 "$PACKAGE_DIR/$path")" "$path"
    done < <(cd "$PACKAGE_DIR" && find . -type f ! -name SHA256SUMS -print | sed 's#^./##' | LC_ALL=C sort)
} >"$PACKAGE_DIR/SHA256SUMS"

tar -C "$DIST_ROOT" -czf "$ARCHIVE_PATH" "$PACKAGE_NAME"
printf 'macOS package ready:\n  app:     %s\n  archive: %s\n' "$APP_DIR" "$ARCHIVE_PATH"

[[ "$MAKE_DMG" == 1 ]] && printf '  dmg:     %s\n' "$DMG_PATH"
if [[ -z "$NOTARY_PROFILE" ]]; then printf 'Notarization was not requested; this build is not yet Gatekeeper-ready for downloaded distribution.\n'; fi
