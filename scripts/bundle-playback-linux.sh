#!/usr/bin/env bash
# Bundle libmpv and the non-platform ELF libraries needed by Slicer's Linux
# playback engine.
#
# The input is an existing libmpv shared library (the default is the native
# Ubuntu libmpv2 installation). This script does not install packages and it
# never copies glibc, the ELF interpreter, or vendor GPU drivers. All other
# ELF dependencies are copied beside libmpv and patched with a private
# DT_RUNPATH=$ORIGIN. The resulting directory can therefore be loaded by an
# engine that dlopens libmpv.so.2 by its explicit package-relative path.
set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)

ARCH=$(uname -m)
case "$ARCH" in
    x86_64|amd64) ARCH=x86_64; TARGET=linux-x86_64; EXPECTED_MACHINE='Advanced Micro Devices X86-64' ;;
    aarch64|arm64) ARCH=aarch64; TARGET=linux-aarch64; EXPECTED_MACHINE='AArch64' ;;
    *)
        printf 'error: unsupported Linux architecture: %s\n' "$ARCH" >&2
        exit 2
        ;;
esac

LIBMPV_INPUT=${SLICER_LIBMPV:-}
OUTPUT_DIR=${SLICER_PLAYBACK_OUTPUT:-$ROOT_DIR/build/playback/$TARGET}
SOURCE_CACHE=${SLICER_PLAYBACK_SOURCE_CACHE:-$ROOT_DIR/packaging/playback-source}
PATCHELF=${SLICER_PATCHELF:-patchelf}
ALLOW_MISSING_SOURCES=${SLICER_PLAYBACK_ALLOW_MISSING_SOURCES:-0}
OFFLINE=${SLICER_PLAYBACK_OFFLINE:-0}
CHECK_DIR=
SOURCE_MANIFEST_INPUT=

usage() {
    cat <<'USAGE'
Usage: scripts/bundle-playback-linux.sh [options]

Copy an ELF libmpv runtime into a self-contained playback directory.

Options:
  --libmpv FILE             libmpv.so.2 input (default: ldconfig discovery)
  --output DIR              output directory (default: build/playback/<target>)
  --source-cache DIR        exact distro source archives (default:
                            packaging/playback-source)
  --source-manifest FILE    additional source provenance rows to include
  --offline                 do not try any source acquisition helper
  --allow-missing-sources   development-only; keep a MISSING row in the
                            manifest instead of failing the bundle
  --check DIR               validate an already assembled playback directory
  -h, --help                show this help

The normal package path requires source archives for every GPL-bearing source
package in the copied closure. Source archives are matched by source package
name and exact source version; a Debian .dsc must have every referenced archive
beside it. --allow-missing-sources is intentionally explicit and should not be
used for release artifacts.
USAGE
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

while (($# > 0)); do
    case "$1" in
        --libmpv)
            (($# >= 2)) || die '--libmpv needs a value'
            LIBMPV_INPUT=$2
            shift 2
            ;;
        --output)
            (($# >= 2)) || die '--output needs a value'
            OUTPUT_DIR=$2
            shift 2
            ;;
        --source-cache)
            (($# >= 2)) || die '--source-cache needs a value'
            SOURCE_CACHE=$2
            shift 2
            ;;
        --source-manifest)
            (($# >= 2)) || die '--source-manifest needs a value'
            SOURCE_MANIFEST_INPUT=$2
            shift 2
            ;;
        --offline)
            OFFLINE=1
            shift
            ;;
        --allow-missing-sources)
            ALLOW_MISSING_SOURCES=1
            shift
            ;;
        --check)
            (($# >= 2)) || die '--check needs a directory'
            CHECK_DIR=$2
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
        die 'sha256sum or shasum is required'
    fi
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

is_elf() {
    local file=$1
    readelf -h -- "$file" >/dev/null 2>&1
}

elf_machine() {
    readelf -h -- "$1" | sed -n 's/^  Machine:[[:space:]]*//p' | head -1
}

soname_for() {
    readelf -d -- "$1" 2>/dev/null |
        sed -n 's/.*Library soname: \[\([^]]*\)\].*/\1/p' | head -1
}

needed_for() {
    readelf -d -- "$1" 2>/dev/null |
        sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p'
}

is_platform_name() {
    case "$1" in
        linux-vdso*|ld-linux*|ld64.so*|libc.so*|libm.so*|libmvec.so*|libpthread.so*|libdl.so*|librt.so*|libresolv.so*|libnsl.so*|libutil.so*|libcrypt.so*|libanl.so*|libBrokenLocale.so*|libnss_*.so*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

is_gpu_driver_path() {
    local path=$1 base
    base=$(basename -- "$path")
    case "$path" in
        */dri/*|*/vulkan/*|*/egl/*)
            return 0
            ;;
    esac
    case "$base" in
        libGLX_mesa*|libEGL_mesa*|libGLX_nvidia*|libEGL_nvidia*|libnvidia*|libcuda*|libvulkan_*|libVkLayer_*|libVkICD_*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

is_gpu_driver_name() {
    case "$(basename -- "$1")" in
        libGLX_mesa*|libEGL_mesa*|libGLX_nvidia*|libEGL_nvidia*|libnvidia*|libcuda*|libvulkan_*|libVkLayer_*|libVkICD_*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

find_ldconfig_library() {
    local name=$1 found
    found=$(ldconfig -p 2>/dev/null |
        awk -v name="$name" -v arch="$ARCH" \
            '$1 == name && (($0 ~ /x86-64/ && arch == "x86_64") || ($0 ~ /aarch64/ && arch == "aarch64")) { print $NF; exit }')
    [[ -n "$found" && -e "$found" ]] || return 1
    realpath -e -- "$found"
}

resolve_library() {
    local name=$1 from=$2 candidate dir package_path
    # A dependency already copied to the staging directory always wins. This
    # also makes resolution deterministic when a host has several candidates.
    if [[ -n "${REAL_BY_NAME[$name]:-}" ]]; then
        printf '%s\n' "${REAL_BY_NAME[$name]}"
        return 0
    fi
    dir=$(dirname -- "$from")
    if [[ -e "$dir/$name" ]]; then
        realpath -e -- "$dir/$name"
        return 0
    fi
    candidate=$(find_ldconfig_library "$name" || true)
    if [[ -n "$candidate" ]]; then
        printf '%s\n' "$candidate"
        return 0
    fi
    # A few distro libraries (for example PulseAudio's libpulsecommon) live in
    # a private subdirectory and intentionally do not appear in ldconfig's
    # public cache. Ask dpkg for the exact installed path before giving up.
    package_path=$(dpkg-query -S "*/$name" 2>/dev/null |
        sed -n '1s/.*: //p' | awk -v arch="$ARCH" '
            ($0 ~ "/" arch "/" || $0 ~ "/" arch "-") { print; exit }
        ' || true)
    if [[ -n "$package_path" && -e "$package_path" ]]; then
        realpath -e -- "$package_path"
        return 0
    fi
    return 1
}

validate_input() {
    [[ -n "$LIBMPV_INPUT" ]] || {
        LIBMPV_INPUT=$(find_ldconfig_library libmpv.so.2 || true)
    }
    [[ -n "$LIBMPV_INPUT" ]] || {
        die 'libmpv.so.2 was not found; pass --libmpv FILE or install a libmpv runtime'
    }
    [[ -f "$LIBMPV_INPUT" || -L "$LIBMPV_INPUT" ]] ||
        die "libmpv input does not exist: $LIBMPV_INPUT"
    LIBMPV_INPUT=$(realpath -e -- "$LIBMPV_INPUT") ||
        die "unable to resolve libmpv input: $LIBMPV_INPUT"
    is_elf "$LIBMPV_INPUT" || die "libmpv input is not an ELF object: $LIBMPV_INPUT"
    [[ "$(elf_machine "$LIBMPV_INPUT")" == "$EXPECTED_MACHINE" ]] ||
        die "libmpv architecture does not match this host: $(elf_machine "$LIBMPV_INPUT")"
    [[ "$(soname_for "$LIBMPV_INPUT")" == 'libmpv.so.2' ]] ||
        die "libmpv input has no SONAME libmpv.so.2: $LIBMPV_INPUT"
}

safe_name() {
    printf '%s' "$1" | sed 's/[^A-Za-z0-9_.+@-]/_/g'
}

package_owner_for() {
    local path=$1 owner
    owner=$(dpkg-query -S -- "$path" 2>/dev/null | sed -n '1s/: .*//p' || true)
    if [[ -z "$owner" ]]; then
        owner=$(dpkg-query -S -- "$(realpath -e -- "$path")" 2>/dev/null |
            sed -n '1s/: .*//p' || true)
    fi
    printf '%s\n' "$owner"
}

package_field() {
    local package=$1 field=$2 value
    [[ -n "$package" ]] || return 0
    value=$(dpkg-query -W -f="\${$field}" -- "$package" 2>/dev/null || true)
    printf '%s\n' "$value"
}

copyright_for_package() {
    local package=$1 doc base
    base=${package%%:*}
    doc=$(dpkg-query -L -- "$package" 2>/dev/null |
        awk '/\/copyright$/ { print; exit }' || true)
    if [[ -z "$doc" && -f "/usr/share/doc/$base/copyright" ]]; then
        doc="/usr/share/doc/$base/copyright"
    fi
    [[ -f "$doc" ]] || return 1
    printf '%s\n' "$doc"
}

license_class_for() {
    local copyright=$1 package=$2
    # A Debian copyright file often contains several per-file licenses. In
    # particular, matching GPL-3 also matches the LGPL-3 spelling, so do not
    # pretend a package-wide SPDX expression can be inferred with grep. The
    # complete notice is shipped and every source package is required below.
    if grep -Eiq 'GPL|LGPL' "$copyright"; then
        printf 'copyleft/mixed; see complete copyright\n'
    else
        printf 'per-file; see complete copyright\n'
    fi
}

apt_mirror() {
    if [[ -n "${SLICER_APT_MIRROR:-}" ]]; then
        printf '%s\n' "$SLICER_APT_MIRROR"
        return
    fi
    awk '$1 == "URIs:" { print $2; exit }' /etc/apt/sources.list.d/ubuntu.sources 2>/dev/null ||
        printf 'unknown\n'
}

apt_suite() {
    if [[ -n "${VERSION_CODENAME:-}" ]]; then
        printf '%s\n' "$VERSION_CODENAME"
    elif [[ -r /etc/os-release ]]; then
        sed -n 's/^VERSION_CODENAME=//p' /etc/os-release | tr -d '"' | head -1
    else
        printf 'unknown\n'
    fi
}

verify_dsc_materials() {
    local dsc=$1 expected size archive path actual actual_size has_sha=0 missing=0
    while IFS=$' \t' read -r expected size archive; do
        [[ -n "$archive" && "$archive" != Files: ]] || continue
        path="$STAGE_DIR/source/$(basename -- "$archive")"
        if [[ ! -f "$path" ]]; then
            candidate=$(find "$SOURCE_CACHE" -type f -name "$(basename -- "$archive")" -print -quit)
            if [[ -n "$candidate" ]]; then
                cp -a -- "$candidate" "$STAGE_DIR/source/"
            else
                printf '%s\n' "$(basename -- "$dsc") missing $archive" >>"$MISSING_SOURCE_ROWS"
                missing=1
                continue
            fi
        fi
        actual=$(sha256 "$path")
        [[ "$actual" == "$expected" ]] ||
            die "SHA-256 mismatch for source file $archive listed by $(basename -- "$dsc")"
        actual_size=$(stat -c '%s' -- "$path" 2>/dev/null || stat -f '%z' -- "$path")
        [[ "$actual_size" == "$size" ]] ||
            die "size mismatch for source file $archive listed by $(basename -- "$dsc")"
        has_sha=1
    done < <(awk '
        /^Checksums-Sha256:/ { in_checksums=1; next }
        in_checksums && /^Files:/ { exit }
        in_checksums && /^[[:space:]]*$/ { exit }
        in_checksums && NF >= 3 { print $1, $2, $3 }
    ' "$dsc")
    (( has_sha == 1 )) || {
        printf '%s\n' "$(basename -- "$dsc") has no Checksums-Sha256 section" >>"$MISSING_SOURCE_ROWS"
        missing=1
    }
    (( missing == 0 ))
}

find_source_materials() {
    local source=$1 version=$2 name escaped candidate base dsc dsc_name dsc_version found=0 dsc_found=0
    [[ -d "$SOURCE_CACHE" ]] || return 1
    name=${source%%:*}
    escaped=${version//:/%3a}
    escaped=${escaped//:/%3A}
    # A source package can carry an epoch in metadata while Debian filenames
    # omit it. Match both spellings, but only the exact source package/version.
    while IFS= read -r -d '' candidate; do
        base=$(basename -- "$candidate")
        case "$base" in
            "${name}_${version}"*|"${name}_${escaped}"*|"${name}_${version#*:}"*|"${name}_${version//:/}"*)
                cp -a -- "$candidate" "$STAGE_DIR/source/"
                found=1
                ;;
        esac
    done < <(find "$SOURCE_CACHE" -type f -print0 | sort -z)
    (( found == 1 )) || return 1

    # A .dsc is a manifest for a source package. Require one and verify every
    # SHA-256/size entry before the runtime can be considered release-ready.
    while IFS= read -r -d '' dsc; do
        dsc_name=$(basename -- "$dsc")
        case "$dsc_name" in
            "${name}_${version}"*|"${name}_${escaped}"*|"${name}_${version#*:}"*|"${name}_${version//:/}"*)
                ;;
            *)
                continue
                ;;
        esac
        dsc_found=1
        dsc_name=$(awk '/^Source:/ { print $2; exit }' "$dsc")
        dsc_version=$(awk '/^Version:/ { print $2; exit }' "$dsc")
        [[ "$dsc_name" == "$name" ]] ||
            die "source manifest $(basename -- "$dsc") names $dsc_name; expected $name"
        [[ "$dsc_version" == "$version" || "$dsc_version" == "${version#*:}" ]] ||
            die "source manifest $(basename -- "$dsc") has version $dsc_version; expected $version"
        verify_dsc_materials "$dsc" || return 1
    done < <(find "$STAGE_DIR/source" -maxdepth 1 -type f -name '*.dsc' -print0)
    (( dsc_found == 1 )) || {
        printf '%s\n' "$source:$version has no .dsc source manifest" >>"$MISSING_SOURCE_ROWS"
        return 1
    }
    return 0
}

source_hash_rows() {
    local path
    while IFS= read -r -d '' path; do
        printf '%s\t%s\n' "$(sha256 "$path")" "$(basename -- "$path")"
    done < <(find "$STAGE_DIR/source" -maxdepth 1 -type f -print0 | sort -z)
}

apt_package_field_for_version() {
    local package=$1 version=$2 field=$3
    # Consume the complete metadata stream. Exiting awk after the first match
    # can SIGPIPE apt-cache and fail the entire bundle under pipefail.
    apt-cache show -- "$package" 2>/dev/null | awk -v want="$version" -v field="$field" '
        /^Package:/ { keep=0 }
        /^Version:/ { keep=($2 == want) }
        !found && keep && index($0, field ":") == 1 { sub("^[^:]*:[[:space:]]*", ""); print; found=1 }
    '
}

copy_notice_for_package() {
    local package=$1 copyright=$2 destination
    destination="$STAGE_DIR/notices/$(safe_name "$package").copyright"
    [[ -f "$destination" ]] || cp -a -- "$copyright" "$destination"
}

check_elf_bundle() {
    local dir=$1 file real dep dep_path missing=0
    [[ -d "$dir" ]] || die "playback bundle does not exist: $dir"
    [[ -e "$dir/libmpv.so.2" ]] || die "playback bundle lacks libmpv.so.2: $dir"
    real=$(realpath -e -- "$dir/libmpv.so.2") || die 'unable to resolve bundled libmpv.so.2'
    is_elf "$real" || die "bundled libmpv.so.2 is not an ELF object: $real"
    [[ "$(soname_for "$real")" == 'libmpv.so.2' ]] ||
        die "bundled object has the wrong SONAME: $real"
    [[ "$(elf_machine "$real")" == "$EXPECTED_MACHINE" ]] ||
        die "bundled libmpv.so.2 has the wrong architecture: $(elf_machine "$real")"
    while IFS= read -r -d '' file; do
        real=$(realpath -e -- "$file")
        if ! is_elf "$real"; then
            continue
        fi
        if ! readelf -d -- "$real" | grep -Fq 'RUNPATH'; then
            printf 'error: bundled ELF lacks DT_RUNPATH: %s\n' "$file" >&2
            missing=1
        elif ! readelf -d -- "$real" | grep -Fq '$ORIGIN'; then
            printf 'error: bundled ELF RUNPATH does not contain $ORIGIN: %s\n' "$file" >&2
            missing=1
        fi
        while IFS= read -r dep; do
            is_platform_name "$dep" && continue
            is_gpu_driver_name "$dep" && continue
            if [[ ! -e "$dir/$dep" ]]; then
                printf 'error: bundled ELF dependency is missing: %s -> %s\n' "$file" "$dep" >&2
                missing=1
            else
                dep_path=$(realpath -e -- "$dir/$dep")
                if is_gpu_driver_path "$dep_path"; then
                    printf 'error: vendor GPU driver was copied into bundle: %s\n' "$dep_path" >&2
                    missing=1
                fi
            fi
        done < <(needed_for "$real")
    done < <(find "$dir" -maxdepth 1 \( -type f -o -type l \) -name 'lib*.so*' -print0 | sort -z)
    (( missing == 0 )) || return 1
}

if [[ -n "$CHECK_DIR" ]]; then
    require_command readelf
    require_command realpath
    [[ -d "$CHECK_DIR" ]] || die "playback bundle does not exist: $CHECK_DIR"
    CHECK_DIR=$(realpath -e -- "$CHECK_DIR")
    check_elf_bundle "$CHECK_DIR"
    printf 'Linux playback bundle check passed: %s\n' "$CHECK_DIR"
    exit 0
fi

require_command readelf
require_command ldconfig
require_command realpath
require_command find
require_command cp
require_command sed
require_command awk
require_command grep
require_command mktemp
require_command "$PATCHELF"
require_command stat
validate_input
[[ -r "$ROOT_DIR/packaging/PLAYBACK-NOTICE.txt" ]] ||
    die "missing playback notice: $ROOT_DIR/packaging/PLAYBACK-NOTICE.txt"

if [[ -e "$OUTPUT_DIR" ]]; then
    die "output already exists; choose another --output (refusing to overwrite): $OUTPUT_DIR"
fi
mkdir -p -- "$(dirname -- "$OUTPUT_DIR")"
STAGE_DIR=$(mktemp -d "$(dirname -- "$OUTPUT_DIR")/.playback-stage.XXXXXX")
cleanup() {
    rm -rf -- "$STAGE_DIR"
}
trap cleanup EXIT INT TERM
mkdir -p -- "$STAGE_DIR/source" "$STAGE_DIR/notices"
MISSING_SOURCE_ROWS="$STAGE_DIR/missing-source.rows"
: >"$MISSING_SOURCE_ROWS"

declare -A SEEN_REAL=()
declare -A REAL_BY_NAME=()
declare -A OWNER_BY_REAL=()
declare -A PACKAGE_COPYRIGHT=()
declare -a QUEUE=()
UNPACKAGED_ROWS="$STAGE_DIR/unpackaged.rows"
: >"$UNPACKAGED_ROWS"

copy_library() {
    local path=$1 real base_real requested soname destination existing
    real=$(realpath -e -- "$path") || die "unable to resolve ELF dependency: $path"
    [[ -f "$real" ]] || die "ELF dependency is not a regular file: $real"
    is_elf "$real" || die "dependency is not ELF: $real"
    [[ "$(elf_machine "$real")" == "$EXPECTED_MACHINE" ]] ||
        die "dependency architecture mismatch: $real ($(elf_machine "$real"))"
    if [[ -n "${SEEN_REAL[$real]:-}" ]]; then
        return
    fi
    base_real=$(basename -- "$real")
    destination="$STAGE_DIR/$base_real"
    if [[ -e "$destination" || -L "$destination" ]]; then
        existing=$(realpath -e -- "$destination" 2>/dev/null || true)
        [[ "$existing" == "$real" ]] ||
            die "ELF filename collision for $base_real: $real and $existing"
    else
        cp -a -- "$real" "$destination"
    fi
    SEEN_REAL[$real]=1
    OWNER_BY_REAL[$real]=$(package_owner_for "$real")
    QUEUE+=("$real")
    REAL_BY_NAME[$base_real]=$real
    soname=$(soname_for "$real")
    if [[ -n "$soname" && "$soname" != "$base_real" ]]; then
        destination="$STAGE_DIR/$soname"
        if [[ -e "$destination" || -L "$destination" ]]; then
            existing=$(realpath -e -- "$destination" 2>/dev/null || true)
            [[ "$existing" == "$real" || "$existing" == "$STAGE_DIR/$base_real" ]] ||
                die "ELF SONAME collision for $soname: $real and $existing"
        else
            ln -s -- "$base_real" "$destination"
        fi
        REAL_BY_NAME[$soname]=$real
    fi
}

copy_library "$LIBMPV_INPUT"
for ((queue_index=0; queue_index<${#QUEUE[@]}; queue_index++)); do
    current=${QUEUE[$queue_index]}
    while IFS= read -r dep; do
        [[ -n "$dep" ]] || continue
        if is_platform_name "$dep"; then
            continue
        fi
        dep_path=$(resolve_library "$dep" "$current" || true)
        [[ -n "$dep_path" ]] || die "unable to resolve libmpv dependency $dep needed by $current"
        if is_gpu_driver_path "$dep_path"; then
            printf 'skipping vendor GPU driver dependency %s (%s)\n' "$dep" "$dep_path"
            continue
        fi
        copy_library "$dep_path"
    done < <(needed_for "$current")
done

printf 'copied %d private ELF objects for libmpv.so.2\n' "${#SEEN_REAL[@]}"

# All private objects need a relative lookup path. Use DT_RUNPATH instead of
# mutating process-wide LD_LIBRARY_PATH; this keeps FFmpeg child commands and
# other shared libraries in the Slicer process isolated from playback.
for ((queue_index=0; queue_index<${#QUEUE[@]}; queue_index++)); do
    current=${QUEUE[$queue_index]}
    "$PATCHELF" --set-rpath '$ORIGIN' "$STAGE_DIR/$(basename -- "$current")"
done

# Collect exact package metadata and complete copyright files. A dependency
# from a locally built libmpv may have no dpkg owner; in that case the source
# manifest records the path/hash and release packaging stops unless the caller
# explicitly opts into the development escape hatch.
declare -A SEEN_PACKAGES=()
for current in "${!OWNER_BY_REAL[@]}"; do
    owner=${OWNER_BY_REAL[$current]}
    if [[ -z "$owner" ]]; then
        if [[ "$ALLOW_MISSING_SOURCES" != 1 ]]; then
            die "no Debian package owner for copied ELF $current; provide a packaged source manifest or use --allow-missing-sources for development only"
        fi
        printf 'UNPACKAGED | %s | %s | MISSING\n' "$current" "$(sha256 "$current")" >>"$UNPACKAGED_ROWS"
        continue
    fi
    if [[ -z "${SEEN_PACKAGES[$owner]:-}" ]]; then
        SEEN_PACKAGES[$owner]=1
        copyright=$(copyright_for_package "$owner" || true)
        [[ -n "$copyright" ]] || die "no Debian copyright file for $owner"
        PACKAGE_COPYRIGHT[$owner]=$copyright
        copy_notice_for_package "$owner" "$copyright"
    fi
done

# Add the top-level playback notice after package-specific copyright files.
cp -a -- "$ROOT_DIR/packaging/PLAYBACK-NOTICE.txt" "$STAGE_DIR/PLAYBACK-NOTICE.txt"

# Source materials are intentionally looked up by source package/version, not
# by whatever happens to be current in an apt mirror. This makes a copied
# distro binary auditable and gives GPL recipients the corresponding source.
SOURCE_ROWS="$STAGE_DIR/SOURCE-MANIFEST.txt"
{
    printf '# Slicer Linux playback source manifest\n'
    printf '# Generated by scripts/bundle-playback-linux.sh\n'
    printf '# Repository: %s\n' "$(apt_mirror)"
    printf '# Suite: %s\n' "$(apt_suite)"
    printf '# Columns: binary-package | binary-version | binary-sha256 | source-package | source-version | architecture | license-index | source-material\n'
} >"$SOURCE_ROWS"

while IFS= read -r owner; do
    [[ -n "$owner" ]] || continue
    binary_version=$(package_field "$owner" Version)
    source_name=$(package_field "$owner" 'source:Package')
    source_version=$(package_field "$owner" 'source:Version')
    [[ -n "$source_name" && -n "$source_version" ]] || {
        source_name=$owner
        source_version=$binary_version
    }
    architecture=$(package_field "$owner" Architecture)
    binary_sha=$(apt_package_field_for_version "$owner" "$binary_version" SHA256)
    [[ -n "$binary_sha" ]] || binary_sha='unknown (package metadata unavailable)'
    copyright=${PACKAGE_COPYRIGHT[$owner]}
    license_index=$(license_class_for "$copyright" "$owner")
    material_status='MISSING'
    if find_source_materials "$source_name" "$source_version"; then
        material_status='present'
    elif [[ "$ALLOW_MISSING_SOURCES" != 1 ]]; then
        printf 'error: source archives for %s (%s %s) are missing from %s\n' \
            "$source_name" "$source_version" "$owner" "$SOURCE_CACHE" >&2
        printf '       pre-seed --source-cache with the exact .dsc/.orig/.debian archives or use --allow-missing-sources for development only\n' >&2
        exit 1
    fi
    printf '%s | %s | %s | %s | %s | %s | %s | %s\n' \
        "$owner" "$binary_version" "$binary_sha" "$source_name" "$source_version" \
        "$architecture" "$license_index" "$material_status" >>"$SOURCE_ROWS"
done < <(printf '%s\n' "${!SEEN_PACKAGES[@]}" | LC_ALL=C sort)

if [[ -s "$UNPACKAGED_ROWS" ]]; then
    {
        printf '\n# Unpackaged ELF inputs (development override)\n'
        cat -- "$UNPACKAGED_ROWS"
    } >>"$SOURCE_ROWS"
fi

if [[ -s "$SOURCE_MANIFEST_INPUT" ]]; then
    printf '\n# Additional caller-supplied source manifest: %s\n' "$SOURCE_MANIFEST_INPUT" >>"$SOURCE_ROWS"
    cat -- "$SOURCE_MANIFEST_INPUT" >>"$SOURCE_ROWS"
fi

if [[ -s "$MISSING_SOURCE_ROWS" ]]; then
    {
        printf '\n# Missing source rows (development override)\n'
        cat -- "$MISSING_SOURCE_ROWS"
    } >>"$SOURCE_ROWS"
fi

# A compact hash list makes it possible to audit a package without trusting
# filenames. The source manifest above still records package/version mapping.
{
    printf '\n# Source material SHA-256\n'
    source_hash_rows
} >>"$SOURCE_ROWS"

PROVENANCE="$STAGE_DIR/PROVENANCE.txt"
LIBMPV_VERSION=$(dpkg-query -W -f='${Version}' libmpv2 2>/dev/null || true)
[[ -n "$LIBMPV_VERSION" ]] || LIBMPV_VERSION='unknown (non-dpkg input)'
HOST_RELEASE=$(tr '\n' ' ' </etc/os-release 2>/dev/null || true)
{
    printf 'Slicer Linux playback bundle provenance\n'
    printf '=========================================\n\n'
    printf 'Generated (UTC): %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    printf 'Target: %s (%s)\n' "$TARGET" "$EXPECTED_MACHINE"
    printf 'Input libmpv: %s\n' "$LIBMPV_INPUT"
    printf 'Input SHA-256: %s\n' "$(sha256 "$LIBMPV_INPUT")"
    printf 'Input SONAME: %s\n' "$(soname_for "$LIBMPV_INPUT")"
    printf 'Input package version: %s\n' "$LIBMPV_VERSION"
    printf 'Repository mirror: %s\n' "$(apt_mirror)"
    printf 'Repository suite: %s\n' "$(apt_suite)"
    printf 'Host release: %s\n' "$HOST_RELEASE"
    printf 'patchelf: %s\n' "$($PATCHELF --version 2>&1 | head -1)"
    printf 'Dependency objects copied: %d\n' "${#SEEN_REAL[@]}"
    printf 'Loader policy: DT_RUNPATH=$ORIGIN on every copied ELF object\n'
    printf 'Excluded: glibc, ELF interpreter, kernel interfaces, and vendor GPU driver paths\n'
    printf 'Source cache: %s\n' "$SOURCE_CACHE"
    if [[ "$ALLOW_MISSING_SOURCES" == 1 ]]; then
        printf 'WARNING: missing source archives were allowed for development; this is not a release artifact\n'
    else
        printf 'Source policy: exact source package archives required\n'
    fi
    printf '\nDependency closure (input path -> package owner):\n'
    for current in "${!OWNER_BY_REAL[@]}"; do
        printf '%s -> %s\n' "$current" "${OWNER_BY_REAL[$current]:-unpackaged input}"
    done | LC_ALL=C sort
} >"$PROVENANCE"

# Copy the staged tree atomically only after all validation and provenance
# generation has succeeded. This keeps a failed build from leaving a
# directory which the package script could mistake for a ready runtime.
check_elf_bundle "$STAGE_DIR"
mv -- "$STAGE_DIR" "$OUTPUT_DIR"
trap - EXIT INT TERM

printf 'Linux playback bundle ready:\n  directory: %s\n  libmpv:    %s\n  objects:   %d\n' \
    "$OUTPUT_DIR" "$OUTPUT_DIR/libmpv.so.2" "${#SEEN_REAL[@]}"
