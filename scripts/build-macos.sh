#!/usr/bin/env bash
# Build the desktop app, bundle private media tools, and produce a DMG.
set -euo pipefail
ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT_DIR"
[[ $(uname -s) == Darwin ]] || { printf 'error: run on macOS\n' >&2; exit 1; }
ARCH=$(uname -m)
case "$ARCH" in arm64) TARGET=macos-aarch64 ;; x86_64) TARGET=macos-x86_64 ;; *) exit 2 ;; esac
LIBMPV=${SLICER_MPV_LIBRARY:-}
PLAYBACK=${SLICER_PLAYBACK_BUNDLE:-$ROOT_DIR/build/playback/$TARGET}
DIST=${SLICER_DIST_ROOT:-$ROOT_DIR/dist}
IDENTITY=${SLICER_SIGN_IDENTITY:--}
PROFILE=${SLICER_NOTARY_PROFILE:-}
DEVELOPMENT=0
usage() {
    cat <<'USAGE'
Usage: scripts/build-macos.sh [options]
  --libmpv FILE          explicit libmpv.2.dylib to bundle (no runtime PATH fallback)
  --playback-bundle DIR  already relocated playback closure
  --dist DIR            output directory (default: dist)
  --sign-identity ID    optional Developer ID Application certificate
  --notary-profile NAME saved notarytool profile; notarize and staple app and DMG
  --development         allow missing corresponding playback sources, for local testing

Builds the full desktop release, app icon, bundled export and playback runtimes,
and a drag-to-Applications DMG. Existing generated artifacts are backed up.
Without signing credentials it uses ad hoc signatures, requiring no Apple account.
Downloaded distribution without Gatekeeper overrides requires Developer ID + notarization.
USAGE
}
while (($#)); do
    case "$1" in
        --libmpv|--playback-bundle|--dist|--sign-identity|--notary-profile)
            (($# >= 2)) || { printf 'error: %s needs a value\n' "$1" >&2; exit 2; }
            case "$1" in
                --libmpv) LIBMPV=$2 ;;
                --playback-bundle) PLAYBACK=$2 ;;
                --dist) DIST=$2 ;;
                --sign-identity) IDENTITY=$2 ;;
                --notary-profile) PROFILE=$2 ;;
            esac
            shift 2 ;;
        --development) DEVELOPMENT=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done
if [[ ! -f "$PLAYBACK/libmpv.2.dylib" ]]; then
    [[ -n "$LIBMPV" ]] || { printf 'error: supply --libmpv FILE or --playback-bundle DIR\n' >&2; exit 1; }
    BUNDLE_ARGS=(--libmpv "$LIBMPV" --output "$PLAYBACK" --arch "$ARCH")
    [[ "$DEVELOPMENT" == 1 ]] && BUNDLE_ARGS+=(--allow-missing-sources)
    python3 scripts/bundle-playback-macos.py "${BUNDLE_ARGS[@]}"
fi
# App and static media tools target macOS 11 by default. The supplied libmpv
# closure can require a newer version, which the packager records truthfully.
FFMPEG_BUNDLE=${SLICER_FFMPEG_BUNDLE:-$ROOT_DIR/build/ffmpeg/$TARGET}
if [[ ! -x "$FFMPEG_BUNDLE/bin/ffmpeg" ]] || ! "$FFMPEG_BUNDLE/bin/ffmpeg" -hide_banner -encoders 2>/dev/null | awk '$2 == "libopenh264" { found=1 } END { exit !found }'; then
    scripts/build-ffmpeg.sh --target "$TARGET" --output "$FFMPEG_BUNDLE"
fi
export MACOSX_DEPLOYMENT_TARGET=${SLICER_MACOS_DEPLOYMENT_TARGET:-11.0}
cargo build --release --locked
python3 tools/collect-rust-notices.py build/rust-notices
mkdir -p "$DIST"
STAGING=$(mktemp -d "$DIST/.macos-build.XXXXXX")
# Preserve failed build artifacts and diagnostics for inspection.
PACKAGE_NAME=slicer-$TARGET
ARGS=(--binary target/release/slicer --ffmpeg-bundle "$FFMPEG_BUNDLE" --playback-bundle "$PLAYBACK" --dist "$STAGING" --name "$PACKAGE_NAME" --sign-identity "$IDENTITY" --dmg)
[[ "$DEVELOPMENT" == 1 ]] && ARGS+=(--allow-missing-playback-sources)
[[ -n "$PROFILE" ]] && ARGS+=(--notary-profile "$PROFILE")
scripts/package-macos.sh "${ARGS[@]}"
BACKUP="$DIST/previous/$PACKAGE_NAME-$(date +%Y%m%d-%H%M%S)"
for name in "$PACKAGE_NAME" "$PACKAGE_NAME.tar.gz" "$PACKAGE_NAME.dmg"; do
    if [[ -e "$DIST/$name" ]]; then
        mkdir -p "$BACKUP"
        mv "$DIST/$name" "$BACKUP/$name"
    fi
    mv "$STAGING/$name" "$DIST/$name"
done
rmdir "$STAGING"
printf '\nDesktop app: %s/%s/Slicer.app\nDMG: %s/%s.dmg\n' "$DIST" "$PACKAGE_NAME" "$DIST" "$PACKAGE_NAME"
