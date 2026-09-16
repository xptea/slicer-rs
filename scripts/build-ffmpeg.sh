#!/usr/bin/env bash
# Build the small, LGPL-only FFmpeg tool pair used by Slicer.
#
# The script never looks up ffmpeg on PATH. It downloads (or accepts) the
# exact upstream source archive named in packaging/ffmpeg.lock, verifies its
# SHA-256, builds from a clean temporary tree, and writes ffmpeg/ffprobe plus
# the source materials to the requested output directory.
set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
LOCK_FILE="$ROOT_DIR/packaging/ffmpeg.lock"

if [[ ! -r "$LOCK_FILE" ]]; then
    printf 'error: missing FFmpeg lock file: %s\n' "$LOCK_FILE" >&2
    exit 1
fi
# shellcheck disable=SC1090
source "$LOCK_FILE"

TARGET=${SLICER_FFMPEG_TARGET:-linux-x86_64}
OUTPUT_DIR=${SLICER_FFMPEG_OUTPUT:-$ROOT_DIR/build/ffmpeg/$TARGET}
CACHE_DIR=${SLICER_FFMPEG_CACHE_DIR:-$ROOT_DIR/build/ffmpeg/cache}
SOURCE_ARCHIVE_PATH=${SLICER_FFMPEG_SOURCE_ARCHIVE_PATH:-}
SOURCE_TREE=${SLICER_FFMPEG_SOURCE_DIR:-}
JOBS=${SLICER_FFMPEG_JOBS:-}
OFFLINE=${SLICER_FFMPEG_OFFLINE:-0}
VERIFY_SIGNATURE=${SLICER_FFMPEG_VERIFY_SIGNATURE:-0}
KEEP_TEMP=${SLICER_FFMPEG_KEEP_TEMP:-0}
STATIC_LINUX=0

usage() {
    cat <<'USAGE'
Usage: scripts/build-ffmpeg.sh [options]

Build the pinned, native-codec LGPL FFmpeg bundle. The result is written as:
  <output>/bin/ffmpeg
  <output>/bin/ffprobe
  <output>/source/ffmpeg-<version>.tar.xz

Options:
  --target TARGET           linux-x86_64 (default), linux-aarch64,
                            macos-x86_64, macos-aarch64, windows-x86_64
  --output DIR              Build output directory
  --cache DIR               Source download cache
  --source-archive FILE     Use this already-downloaded source archive
  --source-dir DIR          Build from this unpacked source tree
  --jobs N                  Number of make jobs
  --offline                 Do not download an archive, signature, or key
  --verify-signature        Verify the upstream PGP signature in a temp keyring
  --static-linux            Link libc statically as well as FFmpeg libraries
  --no-static-linux         Keep the default dynamic system libc/zlib link
  --keep-temp               Keep the temporary source/build tree for debugging
  -h, --help                Show this help

Environment:
  SLICER_FFMPEG_CC and SLICER_FFMPEG_CROSS_PREFIX select a cross toolchain.
  SOURCE_DATE_EPOCH defaults to 0 for repeatable source builds.
USAGE
}

while (($# > 0)); do
    case "$1" in
        --target)
            (($# >= 2)) || { printf 'error: --target needs a value\n' >&2; exit 2; }
            TARGET=$2
            shift 2
            ;;
        --output)
            (($# >= 2)) || { printf 'error: --output needs a value\n' >&2; exit 2; }
            OUTPUT_DIR=$2
            shift 2
            ;;
        --cache)
            (($# >= 2)) || { printf 'error: --cache needs a value\n' >&2; exit 2; }
            CACHE_DIR=$2
            shift 2
            ;;
        --source-archive)
            (($# >= 2)) || { printf 'error: --source-archive needs a value\n' >&2; exit 2; }
            SOURCE_ARCHIVE_PATH=$2
            shift 2
            ;;
        --source-dir)
            (($# >= 2)) || { printf 'error: --source-dir needs a value\n' >&2; exit 2; }
            SOURCE_TREE=$2
            shift 2
            ;;
        --jobs)
            (($# >= 2)) || { printf 'error: --jobs needs a value\n' >&2; exit 2; }
            JOBS=$2
            shift 2
            ;;
        --offline)
            OFFLINE=1
            shift
            ;;
        --verify-signature)
            VERIFY_SIGNATURE=1
            shift
            ;;
        --no-static-linux)
            STATIC_LINUX=0
            shift
            ;;
        --static-linux)
            STATIC_LINUX=1
            shift
            ;;
        --keep-temp)
            KEEP_TEMP=1
            shift
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

case "$TARGET" in
    linux-x86_64|linux-aarch64|macos-x86_64|macos-aarch64|windows-x86_64) ;;
    *)
        printf 'error: unsupported target %s\n' "$TARGET" >&2
        exit 2
        ;;
esac

PROGRAM_SUFFIX=
if [[ "$TARGET" == windows-* ]]; then
    PROGRAM_SUFFIX=.exe
fi

if [[ -z "$JOBS" ]]; then
    JOBS=$(getconf _NPROCESSORS_ONLN 2>/dev/null || printf '2')
fi
if [[ ! "$JOBS" =~ ^[1-9][0-9]*$ ]]; then
    printf 'error: --jobs must be a positive integer\n' >&2
    exit 2
fi

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
    if [[ "$actual" != "$expected" ]]; then
        printf 'error: SHA-256 mismatch for %s\n  expected %s\n  actual   %s\n' \
            "$file" "$expected" "$actual" >&2
        return 1
    fi
    printf 'verified SHA-256 %s  %s\n' "$actual" "$file"
}

download() {
    local url=$1 destination=$2
    command -v curl >/dev/null 2>&1 || {
        printf 'error: curl is required to download %s\n' "$url" >&2
        return 1
    }
    mkdir -p -- "$(dirname -- "$destination")"
    printf 'downloading %s\n' "$url"
    curl --fail --location --retry 3 --proto '=https' --tlsv1.2 \
        --output "$destination.partial" "$url"
    mv -- "$destination.partial" "$destination"
}

mkdir -p -- "$CACHE_DIR" "$OUTPUT_DIR"
if [[ -z "$SOURCE_ARCHIVE_PATH" ]]; then
    SOURCE_ARCHIVE_PATH="$CACHE_DIR/$FFMPEG_SOURCE_ARCHIVE"
fi

if [[ ! -f "$SOURCE_ARCHIVE_PATH" ]]; then
    if [[ "$OFFLINE" == 1 ]]; then
        printf 'error: --offline requested but source archive is missing: %s\n' \
            "$SOURCE_ARCHIVE_PATH" >&2
        exit 1
    fi
    download "$FFMPEG_SOURCE_URL" "$SOURCE_ARCHIVE_PATH"
fi
verify_hash "$SOURCE_ARCHIVE_PATH" "$FFMPEG_SOURCE_SHA256"

SIGNATURE_PATH="$CACHE_DIR/$FFMPEG_SOURCE_ARCHIVE.asc"
if [[ -f "$SIGNATURE_PATH" ]]; then
    verify_hash "$SIGNATURE_PATH" "$FFMPEG_SIGNATURE_SHA256"
elif [[ "$VERIFY_SIGNATURE" == 1 && "$OFFLINE" != 1 ]]; then
    download "$FFMPEG_SIGNATURE_URL" "$SIGNATURE_PATH"
    verify_hash "$SIGNATURE_PATH" "$FFMPEG_SIGNATURE_SHA256"
fi

TEMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/slicer-ffmpeg-build.XXXXXX")
cleanup() {
    if [[ "$KEEP_TEMP" == 1 ]]; then
        printf 'temporary build tree kept at %s\n' "$TEMP_ROOT" >&2
    else
        rm -rf -- "$TEMP_ROOT"
    fi
}
trap cleanup EXIT INT TERM

if [[ "$VERIFY_SIGNATURE" == 1 ]]; then
    [[ -f "$SIGNATURE_PATH" ]] || {
        printf 'error: --verify-signature requested but signature is unavailable: %s\n' \
            "$SIGNATURE_PATH" >&2
        exit 1
    }
    command -v gpg >/dev/null 2>&1 || {
        printf 'error: --verify-signature requires gpg\n' >&2
        exit 1
    }
    GPG_HOME="$TEMP_ROOT/gnupg"
    mkdir -m 700 -- "$GPG_HOME"
    KEY_PATH="$CACHE_DIR/ffmpeg-devel.asc"
    if [[ ! -f "$KEY_PATH" ]]; then
        if [[ "$OFFLINE" == 1 ]]; then
            printf 'error: --offline requested but release key is missing: %s\n' "$KEY_PATH" >&2
            exit 1
        fi
        download "$FFMPEG_RELEASE_KEY_URL" "$KEY_PATH"
    fi
    # Some sandboxed GnuPG installations return status 2 after importing a
    # public key because gpg-agent cannot start. The key is still imported;
    # gpgv below does not need an agent and provides the actual verification.
    gpg --homedir "$GPG_HOME" --batch --quiet --import "$KEY_PATH" || :
    FINGERPRINTS=$(gpg --homedir "$GPG_HOME" --batch --with-colons --fingerprint \
        2>/dev/null | awk -F: '$1 == "fpr" {print toupper($10)}')
    grep -Fxq "$FFMPEG_RELEASE_KEY_FINGERPRINT" <<<"$FINGERPRINTS" || {
        printf 'error: imported release key fingerprint did not match %s\n' \
            "$FFMPEG_RELEASE_KEY_FINGERPRINT" >&2
        exit 1
    }
    GPG_STATUS="$TEMP_ROOT/gpg-status.txt"
    if command -v gpgv >/dev/null 2>&1; then
        gpg --homedir "$GPG_HOME" --batch --export >"$TEMP_ROOT/release-keyring.gpg"
        gpgv --status-fd=1 --keyring "$TEMP_ROOT/release-keyring.gpg" \
            "$SIGNATURE_PATH" "$SOURCE_ARCHIVE_PATH" >"$GPG_STATUS" 2>&1
    else
        gpg --homedir "$GPG_HOME" --batch --status-fd=1 --verify "$SIGNATURE_PATH" \
            "$SOURCE_ARCHIVE_PATH" >"$GPG_STATUS" 2>&1
    fi || {
        cat "$GPG_STATUS" >&2
        printf 'error: FFmpeg release signature verification failed\n' >&2
        exit 1
    }
    grep -Fq "[GNUPG:] VALIDSIG $FFMPEG_RELEASE_KEY_FINGERPRINT " \
        "$GPG_STATUS" || {
        cat "$GPG_STATUS" >&2
        printf 'error: release signature was not made by the pinned key\n' >&2
        exit 1
    }
    printf 'verified FFmpeg release signature with %s\n' "$FFMPEG_RELEASE_KEY_FINGERPRINT"
fi

if [[ -z "$SOURCE_TREE" ]]; then
    SOURCE_TREE="$TEMP_ROOT/source"
    mkdir -p -- "$SOURCE_TREE"
    tar -xJf "$SOURCE_ARCHIVE_PATH" -C "$SOURCE_TREE" --strip-components=1
else
    [[ -x "$SOURCE_TREE/configure" ]] || {
        printf 'error: source directory has no executable configure: %s\n' "$SOURCE_TREE" >&2
        exit 1
    }
fi

BUILD_TREE="$TEMP_ROOT/build"
STAGE_DIR="$TEMP_ROOT/stage"
mkdir -p -- "$BUILD_TREE" "$STAGE_DIR"

if [[ -z "${SOURCE_DATE_EPOCH+x}" ]]; then
    export SOURCE_DATE_EPOCH=0
fi

CONFIGURE_ARGS=(
    "--prefix=$STAGE_DIR"
    --disable-all
    --disable-autodetect
    --disable-network
    --disable-doc
    --disable-debug
    --disable-iconv
    --disable-x86asm
    --disable-shared
    --enable-static
    --enable-ffmpeg
    --enable-ffprobe
    --enable-avcodec
    --enable-avformat
    --enable-avdevice
    --enable-avfilter
    --enable-swscale
    --enable-swresample
    --enable-zlib
    --enable-protocol=file
    --enable-protocol=pipe
    --enable-demuxer=avi
    --enable-demuxer=flac
    --enable-demuxer=ivf
    --enable-demuxer=matroska
    --enable-demuxer=mov
    --enable-demuxer=mpegts
    --enable-demuxer=mp3
    --enable-demuxer=ogg
    --enable-demuxer=wav
    --enable-demuxer=image2
    --enable-demuxer=image2pipe
    --enable-muxer=adts
    --enable-muxer=avi
    --enable-muxer=flac
    --enable-muxer=latm
    --enable-muxer=matroska
    --enable-muxer=mp3
    --enable-muxer=mpegts
    --enable-muxer=mov
    --enable-muxer=mp4
    --enable-muxer=ogg
    --enable-muxer=webm
    # WAV is the PCM container used by waveform extraction and muted audio.
    --enable-muxer=wav
    # GIF stays in the native LGPL profile; its palette pipeline is enabled
    # below so the packaged binary cannot silently fall back to a host build.
    --enable-muxer=gif
    --enable-muxer=null
    --enable-muxer=image2
    --enable-muxer=image2pipe
    --enable-encoder=aac
    --enable-encoder=mpeg4
    --enable-encoder=rawvideo
    --enable-encoder=png
    # Native GIF plus PCM encoders cover GIF export and waveform WAV output.
    --enable-encoder=gif
    # GIF export uses the native encoder/muxer and the palette pipeline below.
    --enable-encoder=pcm_s16le
    --enable-encoder=pcm_s24le
    --enable-encoder=pcm_s32le
    --enable-encoder=pcm_f32le
    --enable-encoder=pcm_s16be
    --enable-encoder=pcm_alaw
    --enable-encoder=pcm_mulaw
    --enable-encoder=wrapped_avframe
    --enable-decoder=aac
    --enable-decoder=ac3
    --enable-decoder=alac
    --enable-decoder=flac
    --enable-decoder=h264
    --enable-decoder=hevc
    --enable-decoder=mjpeg
    --enable-decoder=mp3
    --enable-decoder=mpeg1video
    --enable-decoder=mpeg2video
    --enable-decoder=mpeg4
    --enable-decoder=opus
    --enable-decoder=pcm_alaw
    --enable-decoder=pcm_mulaw
    --enable-decoder=pcm_s16le
    --enable-decoder=pcm_s24le
    --enable-decoder=pcm_s32le
    --enable-decoder=pcm_f32le
    --enable-decoder=pcm_s16be
    --enable-decoder=pcm_u8
    --enable-decoder=rawvideo
    --enable-decoder=vorbis
    --enable-decoder=vp8
    --enable-decoder=vp9
    --enable-decoder=png
    --enable-decoder=webp
    --enable-decoder=wrapped_avframe
    --enable-parser=aac
    --enable-parser=ac3
    --enable-parser=av1
    --enable-parser=h264
    --enable-parser=hevc
    --enable-parser=mpegaudio
    --enable-parser=mpeg4video
    --enable-parser=opus
    --enable-parser=vorbis
    --enable-parser=vp3
    --enable-parser=vp8
    --enable-parser=vp9
    --enable-parser=flac
    --enable-parser=png
    --enable-parser=webp
    --enable-bsf=aac_adtstoasc
    --enable-bsf=h264_mp4toannexb
    --enable-bsf=hevc_mp4toannexb
    --enable-bsf=extract_extradata
    --enable-filter=atrim
    --enable-filter=trim
    --enable-filter=setpts
    --enable-filter=asetpts
    --enable-filter=scale
    --enable-filter=format
    --enable-filter=aresample
    --enable-filter=anull
    --enable-filter=anullsrc
    --enable-filter=null
    --enable-filter=testsrc2
    --enable-filter=sine
    --enable-filter=color
    --enable-filter=fps
    --enable-filter=split
    --enable-filter=palettegen
    --enable-filter=paletteuse
    --enable-filter=copy
    --enable-indev=lavfi
)

case "$TARGET" in
    linux-x86_64)
        CONFIGURE_ARGS+=(--target-os=linux --arch=x86_64)
        ;;
    linux-aarch64)
        CONFIGURE_ARGS+=(--target-os=linux --arch=aarch64)
        ;;
    macos-x86_64)
        CONFIGURE_ARGS+=(--target-os=darwin --arch=x86_64)
        STATIC_LINUX=0
        ;;
    macos-aarch64)
        CONFIGURE_ARGS+=(--target-os=darwin --arch=arm64)
        STATIC_LINUX=0
        ;;
    windows-x86_64)
        CONFIGURE_ARGS+=(
            --target-os=mingw32
            --arch=x86_64
            "--cross-prefix=${SLICER_FFMPEG_CROSS_PREFIX:-x86_64-w64-mingw32-}"
        )
        STATIC_LINUX=0
        ;;
esac

if [[ -n "${SLICER_FFMPEG_CC:-}" ]]; then
    CONFIGURE_ARGS+=("--cc=$SLICER_FFMPEG_CC")
fi
if [[ -n "${SLICER_FFMPEG_CFLAGS:-}" ]]; then
    CONFIGURE_ARGS+=("--extra-cflags=$SLICER_FFMPEG_CFLAGS")
else
    CONFIGURE_ARGS+=(--extra-cflags='-O2 -ffunction-sections -fdata-sections')
fi
if [[ "$TARGET" == linux-* ]]; then
    if [[ "$STATIC_LINUX" == 1 ]]; then
        CONFIGURE_ARGS+=(--extra-ldflags='-static -Wl,--gc-sections' --extra-libs='-lm -lpthread')
    else
        CONFIGURE_ARGS+=(--extra-ldflags='-Wl,--gc-sections' --extra-libs='-lm -lpthread')
    fi
fi
if [[ -n "${SLICER_FFMPEG_LDFLAGS:-}" ]]; then
    CONFIGURE_ARGS+=("--extra-ldflags=$SLICER_FFMPEG_LDFLAGS")
fi

printf 'configuring FFmpeg %s for %s\n' "$FFMPEG_VERSION" "$TARGET"
(
    cd -- "$BUILD_TREE"
    "$SOURCE_TREE/configure" "${CONFIGURE_ARGS[@]}"
    make -j"$JOBS"
)

for program in ffmpeg ffprobe; do
    [[ -x "$BUILD_TREE/$program$PROGRAM_SUFFIX" ]] || {
        printf 'error: FFmpeg build did not produce %s%s\n' "$program" "$PROGRAM_SUFFIX" >&2
        exit 1
    }
done

BIN_DIR="$OUTPUT_DIR/bin"
SOURCE_OUT_DIR="$OUTPUT_DIR/source"
mkdir -p -- "$BIN_DIR" "$SOURCE_OUT_DIR"
install -m 755 "$BUILD_TREE/ffmpeg$PROGRAM_SUFFIX" "$BIN_DIR/ffmpeg$PROGRAM_SUFFIX"
install -m 755 "$BUILD_TREE/ffprobe$PROGRAM_SUFFIX" "$BIN_DIR/ffprobe$PROGRAM_SUFFIX"
SOURCE_ARCHIVE_DEST="$SOURCE_OUT_DIR/$FFMPEG_SOURCE_ARCHIVE"
# A rebuild is often pointed at the archive already stored in the output
# bundle. GNU install rejects copying a file over itself, so retain that
# verified file in place while still copying archives supplied elsewhere.
if [[ "$(realpath -e -- "$SOURCE_ARCHIVE_PATH")" != "$(realpath -m -- "$SOURCE_ARCHIVE_DEST")" ]]; then
    install -m 644 "$SOURCE_ARCHIVE_PATH" "$SOURCE_ARCHIVE_DEST"
fi
[[ -f "$SOURCE_TREE/COPYING.LGPLv2.1" ]] || {
    printf 'error: FFmpeg source tree lacks COPYING.LGPLv2.1\n' >&2
    exit 1
}
install -m 644 "$SOURCE_TREE/COPYING.LGPLv2.1" "$OUTPUT_DIR/COPYING.LGPLv2.1"
if [[ -f "$SIGNATURE_PATH" ]]; then
    install -m 644 "$SIGNATURE_PATH" "$SOURCE_OUT_DIR/$FFMPEG_SOURCE_ARCHIVE.asc"
fi
if [[ -f "$CACHE_DIR/ffmpeg-devel.asc" ]]; then
    install -m 644 "$CACHE_DIR/ffmpeg-devel.asc" "$SOURCE_OUT_DIR/ffmpeg-devel.asc"
fi
install -m 644 "$ROOT_DIR/packaging/FFMPEG-NOTICE.txt" "$OUTPUT_DIR/FFMPEG-NOTICE.txt"
install -m 644 "$ROOT_DIR/packaging/ZLIB-NOTICE.txt" "$OUTPUT_DIR/ZLIB-NOTICE.txt"

if [[ "$TARGET" == linux-* && "$STATIC_LINUX" == 1 ]]; then
    for program in ffmpeg ffprobe; do
        if ! file "$BIN_DIR/$program$PROGRAM_SUFFIX" | grep -Fq 'statically linked'; then
            file "$BIN_DIR/$program$PROGRAM_SUFFIX" >&2
            printf 'error: %s is not statically linked\n' "$program" >&2
            exit 1
        fi
    done
fi

CONFIGURATION=$(
    "$BIN_DIR/ffmpeg$PROGRAM_SUFFIX" -hide_banner -version |
        sed -n 's/^configuration: //p'
)
if grep -Eq -- '--enable-(gpl|version3|nonfree|libx264|libvpx|libmp3lame)' <<<"$CONFIGURATION"; then
    printf 'error: build unexpectedly contains a GPL/nonfree/external codec flag\n%s\n' \
        "$CONFIGURATION" >&2
    exit 1
fi

# Validate the executable's compiled-in capabilities as part of the build.
# This catches an old/stale output directory and keeps the source recipe and
# the shipped binary in lockstep with the GIF and waveform call sites.
require_component() {
    local listing=$1 name=$2
    if ! "$BIN_DIR/ffmpeg$PROGRAM_SUFFIX" -hide_banner -"$listing" 2>/dev/null |
        awk -v target="$name" '$2 == target { found=1 } END { exit !found }'; then
        printf 'error: built FFmpeg is missing required %s: %s\n' "$listing" "$name" >&2
        exit 1
    fi
}

require_component encoders gif
require_component muxers gif
for component in fps split palettegen paletteuse anullsrc; do
    require_component filters "$component"
done
require_component muxers wav
for component in pcm_s16le pcm_s24le pcm_s32le pcm_f32le; do
    require_component encoders "$component"
done

BIN_SHA256=$(sha256 "$BIN_DIR/ffmpeg$PROGRAM_SUFFIX")
PROBE_SHA256=$(sha256 "$BIN_DIR/ffprobe$PROGRAM_SUFFIX")
{
    printf 'Slicer FFmpeg build provenance\n'
    printf '==============================\n'
    printf 'Target:                 %s\n' "$TARGET"
    printf 'FFmpeg version:         %s\n' "$FFMPEG_VERSION"
    printf 'FFmpeg release tag:     %s\n' "$FFMPEG_RELEASE_TAG"
    printf 'Source archive:         %s\n' "$FFMPEG_SOURCE_ARCHIVE"
    printf 'Source URL:             %s\n' "$FFMPEG_SOURCE_URL"
    printf 'Source SHA-256:         %s\n' "$FFMPEG_SOURCE_SHA256"
    printf 'Source size:            %s bytes\n' "$FFMPEG_SOURCE_SIZE"
    printf 'Source signature URL:   %s\n' "$FFMPEG_SIGNATURE_URL"
    printf 'Release key fingerprint:%s\n' "$FFMPEG_RELEASE_KEY_FINGERPRINT"
    printf 'License:                %s\n' "$FFMPEG_LICENSE"
    printf 'ffmpeg SHA-256:         %s\n' "$BIN_SHA256"
    printf 'ffprobe SHA-256:        %s\n' "$PROBE_SHA256"
    if [[ "$TARGET" == linux-* && "$STATIC_LINUX" == 1 ]]; then
        printf 'Static Linux link:      yes\n'
    else
        printf 'Static Linux link:      no\n'
    fi
    printf '\nConfigure flags:\n%s\n' "$CONFIGURATION"
} >"$SOURCE_OUT_DIR/PROVENANCE.txt"

printf 'FFmpeg bundle ready: %s\n' "$OUTPUT_DIR"
printf '  %s\n  %s\n' "$BIN_DIR/ffmpeg$PROGRAM_SUFFIX" "$BIN_DIR/ffprobe$PROGRAM_SUFFIX"
