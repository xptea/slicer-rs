# Packaging

Slicer releases carry two separate media runtimes. An installed layout puts
`slicer` in `bin/`, the static FFmpeg command line tools in
`lib/slicer/bin/`, and the native libmpv playback runtime in
`lib/slicer/playback/`. The application resolves the command line tools from
its own executable directory and never falls back to a program named `ffmpeg`
or `ffprobe` on `PATH`. The playback engine opens
`../lib/slicer/playback/libmpv.so.2` explicitly. `SLICER_FFMPEG_DIR` remains an
explicit development and test override for the command line tools.

The two runtimes intentionally have separate loader and license boundaries:
the export/preview tools are statically linked from the source-pinned FFmpeg
LGPL profile below, while libmpv and its shared libraries are private ELF
objects with `DT_RUNPATH=$ORIGIN`. The package does not set a process-wide
`LD_LIBRARY_PATH`, so the playback FFmpeg ABI cannot leak into the export
subprocesses.

The reference Linux build also produces a reduced libmpv profile. It statically
links the pinned FFmpeg decode path into mpv and enables only `libmpv`, GPU
presentation (OpenGL/EGL/Vulkan), VA-API, PulseAudio, X11, libass, and the
local-file playback paths Slicer uses. The command-line player, DVD/Bluray,
Lua/JavaScript, streaming, Wayland, VDPAU, alternate audio backends, and other
optional integrations are disabled. The resulting runtime is about 41 MB on
the reference x86-64 host and carries no `libav*` shared-library dependencies.
When `build/playback-minimal/linux-<arch>` exists, `package-linux-deb.sh`
selects it automatically; `--playback-bundle` can still select a different
runtime when a source-complete distro closure is required.

The application selects OpenGL/EGL for libmpv presentation on Linux by default,
avoiding a second Vulkan device in the same process as GPUI. Set
`SLICER_MPV_GPU_API=vulkan` to compare the alternate presentation path when
diagnosing a driver-specific issue.

## Static FFmpeg source and license

The bundle is built from the official FFmpeg 7.1.5 source release. The lock
file records the URL, archive size, SHA-256, release key URL, and release key
fingerprint:

| Item | Value |
| --- | --- |
| Archive | `ffmpeg-7.1.5.tar.xz` |
| Official URL | <https://ffmpeg.org/releases/ffmpeg-7.1.5.tar.xz> |
| SHA-256 | `de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f` |
| Signature | <https://ffmpeg.org/releases/ffmpeg-7.1.5.tar.xz.asc> |
| Release key | <https://ffmpeg.org/ffmpeg-devel.asc> |
| Key fingerprint | `FCF986EA15E6E293A5644F10B4322F04D67658D8` |
| License | LGPL-2.1-or-later |

The upstream [FFmpeg license documentation](https://ffmpeg.org/doxygen/trunk/md_LICENSE.html)
explains the LGPL default and the effect of enabling GPL, version 3, or
nonfree components. The build passes none of `--enable-gpl`,
`--enable-version3`, or `--enable-nonfree`, and does not link x264, libvpx, or
libmp3lame. FFmpeg's official [download page](https://ffmpeg.org/download.html)
documents the signed source releases and release key.

The static FFmpeg command line tools are one component of a Linux package.
Their LGPL notice does not describe the separate libmpv playback runtime.
Every package includes all of the following under `share/slicer/` (or the
corresponding macOS app Resources directory):

* the exact source archive and `source/PROVENANCE.txt`;
* the archive signature and release key when they were available to the build;
* the extracted `COPYING.LGPLv2.1` and `FFMPEG-NOTICE.txt`;
* the zlib notice used by the PNG preview path; and
* the Rust dependency notices collected by
  `tools/collect-rust-notices.py`.

The source archive is the source provenance for the FFmpeg code in the
artifact. The FFmpeg libraries are linked statically into the two tools. The
default Linux tools dynamically use the platform's `libc`, `libm`, and
`libz.so.1`; these operating-system libraries are supplied by the target Linux
distribution and are not presented as Slicer redistributables. Review the
applicable LGPL static-link requirements before redistributing a modified
combined work. The complete combined-distribution summary is
`share/slicer/COMBINED-DISTRIBUTION-NOTICE.txt`.

The build recipe is source-pinned and records the binary hashes and complete
configure command in `source/PROVENANCE.txt`. It is not a claim that binaries
from different hosts and toolchains are bit-for-bit identical: compiler,
assembler, linker, SDK, and the temporary build prefix can change the output.

## Codec profile

The profile is intentionally small and uses FFmpeg's native codecs:

| Capability | Included components |
| --- | --- |
| Exact video export | native MPEG-4 Part 2 encoder |
| GIF export | native GIF muxer/encoder with `fps`, `split`, `palettegen`, and `paletteuse` |
| Exact audio export | native AAC encoder and PCM encoders |
| Decode | H.264, HEVC, MPEG-1/2/4, MJPEG, VP8/VP9, AAC, AC-3, FLAC, MP3, Opus, Vorbis, and PCM |
| Preview | native PNG encoder, `scale`, `format`, `aresample`, and the trim/timestamp filters |
| Local inputs | MP4/MOV, Matroska/WebM, MPEG-TS, AVI, MP3, WAV, FLAC, Ogg, and image pipes |

There is no native MP3, VP8, or VP9 encoder in this profile. MP3 export is
reported as unsupported by Slicer; exact WebM export also needs an encoder
that is not in this bundle. GIF export uses FFmpeg's native GIF encoder and
the palette generation/use filters listed above, so it does not add an
external codec or GPL component. FFmpeg's [general documentation](https://www.ffmpeg.org/general.html)
describes libmp3lame and libvpx as external encoder libraries. They are not
silently picked up from a host installation. x264 is likewise omitted because
FFmpeg documents it as GPL; adding it would change the license profile.

## Linux native playback runtime

`scripts/bundle-playback-linux.sh` consumes an existing Linux `libmpv.so.2`
and copies its transitive shared-library closure into
`build/playback/linux-<arch>/`. On Ubuntu 26.04 the default input is the
installed `libmpv2` package (currently 0.41.0-2ubuntu4 on the reference
builder). The script resolves dependencies through `ldconfig`, records every
binary package and source package version, and refuses to claim a runtime is
ready when `libmpv.so.2` or its metadata is absent.

The closure is intentionally a shared runtime rather than another FFmpeg
build. It includes codec, subtitle, audio, display, and graphics interface
libraries needed by the installed libmpv. It excludes glibc, the ELF loader,
kernel interfaces, and vendor GPU driver files (`dri/`, Vulkan ICD/layer,
Mesa, NVIDIA, AMD, and Intel driver objects). The target Linux system supplies
those platform and driver interfaces. Every copied ELF object receives
`DT_RUNPATH=$ORIGIN`, which lets the engine's explicit
`dlopen("../lib/slicer/playback/libmpv.so.2")` find the private closure without
installing system libraries or modifying `LD_LIBRARY_PATH`. Do not add the
playback directory to a global launcher environment: export and preview use
the separate static tools in `lib/slicer/bin/`.

The Ubuntu libmpv input is not an LGPL-only artifact. The installed
`libmpv2` copyright file says that its distributed binaries are GPL-3-or-later
because of their linked libraries; Ubuntu's default FFmpeg shared libraries
are GPL-2-or-later according to their package copyright file. Other closure
members carry their own LGPL, BSD, MIT, Apache, ISC, or per-file terms. The
script copies complete Debian copyright files to `notices/` and emits
`SOURCE-MANIFEST.txt`; the package installs these as
`share/slicer/playback-notices/` and
`share/slicer/PLAYBACK-SOURCE-MANIFEST.txt`. The combined MIT application plus
third-party runtime notice is
`share/slicer/COMBINED-DISTRIBUTION-NOTICE.txt`. The top-level
`share/slicer/LICENSE` continues to preserve Slicer's MIT notice.

Release source policy is strict. Before invoking the bundler, populate a
source cache with the exact Debian source materials for every copied source
package: its `.dsc`, `.orig.tar.*`, `.debian.tar.*` or `.diff.*` files as named
by the `.dsc`. The script verifies every SHA-256 and size listed by each `.dsc`
and records hashes for the copied material. A source manifest supplied with
`--source-manifest` can carry repository metadata for locally built inputs.
`--allow-missing-sources` exists only for local development and writes an
explicit warning; `package-linux.sh` does not enable it by default.

For an Ubuntu build host, enable matching `deb-src` entries for the Ubuntu
release, then download exact source versions into the cache without installing
anything. For example, after selecting the same `resolute` repositories used
by the builder:

```sh
mkdir -p packaging/playback-source
(cd packaging/playback-source && apt-get source --download-only \
  mpv=0.41.0-2ubuntu4 ffmpeg=7:8.0.1-3ubuntu2)
```

Repeat this for the other source packages listed by the first bundler run;
their names and versions are pinned in the manifest. A clean build image with
the required `deb-src` indexes is recommended so source retrieval cannot
silently select a newer package. The bundler itself never runs `apt-get`
and never installs a runtime package.

Build prerequisites for this step are `readelf`, `ldconfig`, `patchelf`,
`dpkg-query`, `apt-cache`, and SHA-256 tooling. `patchelf` is required because
an unpatched system `libmpv.so.2` would resolve its dependencies from the host
and would not be a portable package. Validate a completed directory with:

```sh
scripts/bundle-playback-linux.sh --check build/playback/linux-x86_64
```

The check verifies the SONAME and architecture, private `DT_RUNPATH`, closure
presence, and the absence of copied vendor GPU driver objects.

## Build the FFmpeg bundle

Build prerequisites are a POSIX shell, `make`, a C compiler, `tar`, `curl`
(unless an archive is supplied), and SHA-256 tooling (`sha256sum` or
`shasum`). `nasm` is not required because the minimal profile disables x86
assembly. GnuPG is required only when `--verify-signature` is requested.

Build and verify the native Linux bundle from the official archive:

```sh
scripts/build-ffmpeg.sh \
  --target linux-x86_64 \
  --cache build/ffmpeg/cache \
  --verify-signature \
  --jobs 4
```

For an offline build, place the exact archive, its `.asc` signature, and
`ffmpeg-devel.asc` in the cache, then run:

```sh
scripts/build-ffmpeg.sh \
  --target linux-x86_64 \
  --cache build/ffmpeg/cache \
  --source-archive build/ffmpeg/cache/ffmpeg-7.1.5.tar.xz \
  --offline --verify-signature --jobs 4
```

The output is `build/ffmpeg/linux-x86_64/bin/ffmpeg` and `ffprobe`, with the
source materials beside them. `--static-linux` additionally asks the linker
for a static libc. It is an opt-in cross-toolchain mode; the normal package
uses dynamic system runtime libraries so it does not imply redistribution of
glibc or its relink materials.

The same source recipe accepts `macos-x86_64`, `macos-aarch64`, and
`windows-x86_64`. macOS cross builds need the matching Apple SDK/toolchain;
Windows builds need a MinGW cross compiler (the default prefix is
`x86_64-w64-mingw32-`). Set `SLICER_FFMPEG_CC`,
`SLICER_FFMPEG_CROSS_PREFIX`, `SLICER_FFMPEG_CFLAGS`, or
`SLICER_FFMPEG_LDFLAGS` for a target toolchain. The script still verifies the
same source hash and rejects GPL/nonfree/external codec flags.

## Linux package

Collect Rust notices from the exact locked dependency graph before packaging:

```sh
python3 tools/collect-rust-notices.py build/rust-notices
```

Then assemble a package. `--binary` is useful when the UI build is staged in a
custom directory; otherwise the script chooses `target/release/slicer`, then
`target/debug/slicer`.

```sh
scripts/package-linux.sh \
  --binary target/release/slicer \
  --ffmpeg-bundle build/ffmpeg/linux-x86_64 \
  --playback-bundle build/playback/linux-x86_64 \
  --playback-source-cache packaging/playback-source \
  --rust-notices build/rust-notices \
  --dist dist
```

The script refuses to package a bundle without the pinned FFmpeg source
archive, its hash, provenance, extracted LGPL text, the libmpv ELF closure,
the playback source manifest/materials, and Rust notices. It refuses to
overwrite an existing output. The unpacked layout is:

```text
slicer-linux-x86_64/
├── bin/slicer
├── lib/slicer/bin/ffmpeg
├── lib/slicer/bin/ffprobe
├── lib/slicer/playback/libmpv.so.2
├── share/applications/slicer.desktop
├── share/icons/hicolor/256x256/apps/slicer.png
└── share/slicer/
    ├── COMBINED-DISTRIBUTION-NOTICE.txt
    ├── COPYING.LGPLv2.1
    ├── FFMPEG-NOTICE.txt
    ├── PLAYBACK-NOTICE.txt
    ├── PLAYBACK-PROVENANCE.txt
    ├── PLAYBACK-SOURCE-MANIFEST.txt
    ├── ZLIB-NOTICE.txt
    ├── ffmpeg-source/
    ├── playback-notices/
    ├── playback-source/
    ├── rust-notices/
    ├── slicer-source/
    └── packaging.md
```

The launcher and the native X11 window use the same scissors icon from
`resources/icons/hicolor/256x256/apps/slicer.png`.

Run the smoke check on the unpacked directory:

```sh
scripts/smoke-linux-bundle.sh dist/slicer-linux-x86_64
```

It creates a short fixture using the packaged FFmpeg, runs the packaged
ffprobe, and asks the packaged Slicer to produce a PNG preview while `PATH`
contains no media tools. It also checks that `slicer binaries` reports the
package's sibling `lib/slicer/bin` paths.

The default Linux FFmpeg executables should report only normal platform
runtime dependencies such as `libc`, `libm`, `libz`, and the ELF loader when
checked with `ldd`. The presence of a host `ffmpeg` package is neither needed
nor used. The playback closure's `DT_RUNPATH=$ORIGIN` keeps its FFmpeg 8
shared-library ABI private to libmpv; the export tools remain the independent
static FFmpeg 7.1.5 build. The package's `slicer-source/` directory contains
the application source snapshot (excluding build/, dist/, target/, .git, and
userfiles) alongside the MIT notice.

### Debian package

For a production Linux installer, run the Debian packager. It builds the
release binary when one is not already present, assembles the runtime payload,
and writes a lean native package such as
`dist/slicer_0.1.0_amd64.deb`:

```sh
scripts/package-linux-deb.sh \
  --ffmpeg-bundle build/ffmpeg/linux-x86_64 \
  --dist dist
```

The command automatically uses `build/playback-minimal/linux-x86_64` when the
reduced runtime has been built. Pass `--playback-bundle
build/playback/linux-x86_64` to intentionally package the larger distro
closure instead.

The package installs the application under `/usr/lib/slicer`, adds the
`/usr/bin/slicer` launcher, and registers the desktop file and icon in the
standard `/usr/share` locations. Shared-library dependencies are generated
from the bundled ELF closure with `dpkg-shlibdeps`; pass `--depends` when a
distribution-specific dependency policy is preferred. Runtime-only packaging
omits the large FFmpeg/libmpv source archives, source snapshots, and Rust
notice tree; the small license notices remain installed. The Debian packager
does not require a source cache when an already-built playback bundle is
provided. If it must build that bundle from an installed `libmpv`, pass
`--playback-source-cache` for the strict source policy (or explicitly opt into
`--allow-missing-playback-sources` for development). Use
`scripts/package-linux.sh` when a source-complete portable compliance bundle
is required. The existing portable packager also accepts
`scripts/package-linux.sh --deb` and forwards to this script. Existing `.deb`
output is never overwritten.

## macOS package

On a macOS host, build the matching FFmpeg target and package an app bundle.
Use `macos-aarch64` on Apple Silicon or `macos-x86_64` on Intel:

```sh
scripts/build-ffmpeg.sh --target macos-aarch64 --verify-signature --jobs 4
scripts/package-macos.sh \
  --binary target/release/slicer \
  --ffmpeg-bundle build/ffmpeg/macos-aarch64 \
  --rust-notices build/rust-notices
```

The script writes `Slicer.app` inside `dist/slicer-macos-*` and creates a tar
archive. Because Slicer's executable-relative fallback is
`../lib/slicer/bin`, the app uses:

```text
Slicer.app/Contents/MacOS/slicer
Slicer.app/Contents/lib/slicer/bin/{ffmpeg,ffprobe}
Slicer.app/Contents/Resources/slicer/{source,notices,rust-notices}
```

Use `SLICER_MACOS_ARCH=arm64` or `x86_64` when preparing a cross-target app.
The script consumes only an explicit verified bundle and never copies a
system FFmpeg.

## Windows package

Build the Windows bundle from a MinGW shell, then run the PowerShell packager
on a Windows checkout:

```powershell
bash scripts/build-ffmpeg.sh --target windows-x86_64 --verify-signature --jobs 4
pwsh -File scripts/package-windows.ps1 `
  -Binary target\release\slicer.exe `
  -FfmpegBundle build\ffmpeg\windows-x86_64 `
  -RustNotices build\rust-notices
```

The ZIP contains the same installed layout, with `.exe` suffixes:

```text
bin/slicer.exe
lib/slicer/bin/ffmpeg.exe
lib/slicer/bin/ffprobe.exe
share/slicer/{ffmpeg-source,rust-notices,notices}
```

`package-windows.ps1` validates the pinned archive with `Get-FileHash` and
requires the source/provenance/notice materials. It can invoke the checked-in
POSIX build script through `bash` when a bundle is absent, but it never uses a
host `ffmpeg.exe` as a release input.
