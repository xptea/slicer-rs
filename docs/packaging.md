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
* the extracted `COPYING.LGPLv2.1`, `COPYING.OPENH264`, and `FFMPEG-NOTICE.txt`;
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

The profile uses FFmpeg native codecs plus pinned, statically compiled OpenH264:

| Capability | Included components |
| --- | --- |
| Exact video export | OpenH264 H.264 encoder, identical on Linux/Windows/macOS |
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
silently picked up from a host installation. OpenH264 2.6.0 is built from the
pinned BSD-2-Clause source archive, included with its license in every portable
bundle. It requires a C++ toolchain and pkg-config at build time. x264 is omitted because
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
same source hashes and rejects GPL/nonfree/unapproved codec flags.

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
static FFmpeg 7.1.5/OpenH264 build. The package's `slicer-source/` directory contains
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

Build on the matching macOS architecture (Apple Silicon or Intel). The complete
build command produces the desktop release, native `.icns` icon, private runtimes,
app bundle, tar archive, and a DMG with an Applications shortcut:

```sh
scripts/build-macos.sh --libmpv /opt/homebrew/lib/libmpv.2.dylib --development
scripts/smoke-macos-bundle.sh dist/slicer-macos-aarch64/Slicer.app
```

The explicit libmpv path is only a build input. The installed app resolves
`Contents/lib/slicer/playback/libmpv.2.dylib` and does not use Homebrew or PATH.
`bundle-playback-macos.py` follows the full Mach-O dependency closure, checks CPU
architecture, replaces non-system load paths with `@loader_path`, and signs the
modified libraries. Apple system frameworks and `/usr/lib` libraries stay on the
host. The separate static FFmpeg export/thumbnail tools are still built from the
pinned source profile and resolve from `Contents/lib/slicer/bin`.

```text
Slicer.app/Contents/MacOS/slicer
Slicer.app/Contents/lib/slicer/bin/{ffmpeg,ffprobe}
Slicer.app/Contents/lib/slicer/playback/{libmpv.2.dylib,private dylibs,notices}
Slicer.app/Contents/Resources/Slicer.icns
Slicer.app/Contents/Resources/slicer/{SOURCE-DOWNLOAD.txt,rust-notices,license notices}
corresponding-source/{ffmpeg,playback}   # Included in the tar archive only
```

The DMG contains the compressed runtime app, without the large media source
archives. The matching `.tar.gz` contains both the app and those archives beside
it. The app's `SOURCE-DOWNLOAD.txt` links to the exact version's media archive
and Slicer source tag. Keep these sources available when redistributing the app.
Runtime libraries and license notices remain inside the app, so recipients do
not need Homebrew or the source archive to play and export videos.

Playback notices include installed Homebrew license files, receipts, build
formulas, SBOMs, and input hashes. Put the exact corresponding source archives
and build/patch materials for each copied formula into
`packaging/playback-source/macos/<formula>-<installed-version>/`. The source
manifest records their hashes. A release without those materials fails; the
explicit `--development` option permits MISSING rows for local testing. A custom
non-Homebrew runtime requires its own complete source/license provenance.
Source presence and hashes are recorded, but publishers must verify the materials
are the complete corresponding sources for their binaries.

`build-macos.sh` backs up previous generated outputs under `dist/previous/` before
replacing them, so `dist/slicer-macos-aarch64/Slicer.app` and the matching DMG always
contain the latest completed build. The lower-level
`package-macos.sh` accepts `--binary`, `--ffmpeg-bundle`, `--playback-bundle`, and
`--dmg`; it rejects wrong-architecture or incomplete runtime layouts.

### Signing, notarization, and Gatekeeper

By default the script uses ad hoc signatures, requiring no Apple subscription.
This makes a local build runnable, but downloaded copies still require manual
approval on the destination Mac. Quarantine is attached by the downloading app;
removing it while building cannot bake a Gatekeeper bypass into the DMG.

For distribution with default Gatekeeper settings, use a valid Developer ID
Application certificate and a previously saved notarytool keychain profile:

```sh
scripts/build-macos.sh \
  --playback-bundle build/playback/macos-aarch64 \
  --sign-identity 'Developer ID Application: Name (TEAMID)' \
  --notary-profile slicer-notary
```

Nested dylibs and executables are signed inside out with the hardened runtime and
secure timestamps. The script notarizes and staples the app, builds and signs the
DMG, then notarizes/staples the DMG and verifies Gatekeeper acceptance. It stops
on signing, notarization, or assessment errors. Credentials stay in the macOS
keychain; passwords and API keys are never script arguments.
See Apple's [notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).

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

The macOS packager derives `LSMinimumSystemVersion` from the app and every
bundled executable and dylib. Supplying a Homebrew playback library built for
a recent macOS version raises the bundle minimum accordingly. To support older
macOS releases, supply a libmpv closure built for those releases; setting the
app deployment target alone cannot lower its dependencies’ requirements.

Every platform packager checks for `libopenh264` and the pinned OpenH264 source
archive/license. Stale MPEG-4-only export bundles are rebuilt or rejected rather
than being packaged with application code that expects H.264. Software export
uses one encoder and source-derived bitrate policy on all targets; GPU playback
continues to use each platform’s native libmpv surface.

## Release automation and update feed

`.github/workflows/release.yml` runs for `release: published` only and dispatches
`.github/workflows/build-release.yml` using `repository_dispatch` on `main`.
The worker checks out the published release tag. The Linux playback packaging
helper comes from the workflow's pinned commit, allowing packaging repairs
without changing tagged application code or moving a published tag. Its content
is included in the playback cache recipe. Running the workflow on the
default branch allows successive release tags to share GitHub caches, which
otherwise cannot be restored across sibling tags. Pushes do not trigger builds.
Manual runs of **Build release downloads** use the same test/package pipeline.
With `release_tag` empty, they build the selected commit, upload workflow
artifacts, and skip release publishing. With an existing published stable tag,
they build that tag and upload its release assets after validation. Stable
`vMAJOR.MINOR.PATCH` releases build natively
on Ubuntu 24.04 x86_64 and macOS 15 Apple Silicon. Intel Mac and ARM Linux
are excluded from the release matrix. Version validation,
all application/media tests, source collection, runtime relocation, package
smoke checks and macOS signature/DMG checks must pass before the publish job
uploads any assets. Rerunning a failed release workflow replaces that release's
asset filenames. Prereleases skip the production build and updater.

Rust's registry, Git dependencies, and compiled release artifacts are cached per
runner/architecture and toolchain with manifest, lockfile, and vendored-source
invalidation. Media caches retain the complete compiled FFmpeg and playback
closures, including corresponding sources, keyed by platform and build recipe.
Restored runtime binaries and source hashes are verified before reuse. Native
runtime caches are saved before application tests, so a later failure does not
discard those builds. The export runtime is saved before playback collection,
so playback failures also retain the compiled export tools. Separate source
download caches speed up runtime rebuilds. Tests use `cargo test --release`,
sharing dependencies with packaging.
Rust builds the release executable and test executables while playback inventory
and source collection run in parallel on the same runner. Media tests run after
both finish; a failed preparation stops the other process group before cache
saving. The completed playback cache is keyed by the runtime recipe, independent
of download/orchestration helper edits, with an exact legacy-cache migration.
The CI compiler is pinned to Rust 1.99.0. Compiler upgrades are explicit workflow
changes, preserving compiled dependencies between runs of the same toolchain.
CI removes the runner's unused floating `stable` toolchain after selecting the
pin, because rust-cache fingerprints all installed compilers, not just the one
used for this build.
The first build is cold; GitHub can evict old caches, so cache reuse is an
optimization rather than a prerequisite for a successful build.
Each build has a 60-minute limit; playback inventory and source collection have
a 30-minute limit. Rust notices are collected before native builds, with locked
Cargo metadata allowed to fetch platform-specific crates absent from a native
build's cache.

The Linux job enables distro source repositories and downloads the exact
`.dsc`/upstream/distro archives named by the copied ELF dependency inventory.
The strict Linux bundler checks their versions, sizes and SHA-256 hashes.
Source downloads use the official Ubuntu archive, run four at a time with
bounded retries/timeouts, and keep separate per-package download directories.
Completed archives are exposed at the cache root for the strict bundler;
partial downloads are retained in the source cache even if a later step fails.
The macOS job reads the saved installed Homebrew formulas, source recipes,
resources and patch SBOMs. It downloads checksum-verified archives and exact
Git revisions, including submodules, and verifies the complete collected source
indexes when bundling. Its source cache can also be prepared locally:

```sh
python3 scripts/collect-playback-sources.py macos \
  build/playback/macos-aarch64 packaging/playback-source/macos
python3 scripts/bundle-playback-macos.py \
  --libmpv /opt/homebrew/lib/libmpv.2.dylib \
  --output build/playback-release/macos-aarch64 \
  --source-cache packaging/playback-source/macos --require-source-index
scripts/build-macos.sh --playback-bundle build/playback-release/macos-aarch64
```

Release filenames are stable, with the version in the GitHub release tag:

| Platform | Downloads |
| --- | --- |
| Apple Silicon | `slicer-macos-aarch64.dmg`, `.tar.gz` |
| Linux x86_64 | `slicer-linux-x86_64.tar.gz`, `.deb` |

Each release also receives `version.json` and `SHA256SUMS.txt`. Update
`Cargo.toml`, `Cargo.lock` and the root `version.json` together on `main`, then
publish a release tagged `v` plus that version from the same commit. The app
fetches `https://raw.githubusercontent.com/xptea/slicer-rs/main/version.json`
once per launch with a 10-second request timeout and bounded response sizes.
It compares semantic versions, ignores prereleases/drafts, and checks the latest
GitHub release for a completely uploaded, nonempty asset of the matching version
and OS/CPU. Home then offers a Download button which opens that exact GitHub asset.
The app does not download or replace its executable automatically. Settings
always displays the compiled local package version. The repository and releases
must be public; credentials are never embedded in the app.

The default release workflow uses account-free ad hoc macOS signing. It removes
build-machine extended attributes before signing and includes
`packaging/MACOS-INSTALL.txt` inside the DMG. A recipient's browser can apply
quarantine again: a DMG cannot remove Gatekeeper requirements on another Mac.
The instructions explain one-time approval for this app, including the targeted
quarantine command if Privacy & Security does not offer Open Anyway. Developer
ID and notarization remain optional through the macOS scripts' existing
`--sign-identity` / `--notary-profile` flags.

Local builds inherit the minimum macOS version of their playback dependencies.
Building on a newer Mac with newer Homebrew bottles can require a newer macOS
than the hosted release build; the packager computes this requirement and writes
it to `LSMinimumSystemVersion` rather than claiming unsupported compatibility.
