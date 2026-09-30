# Slicer

Native Linux and macOS video trimming toolbox built with GPUI Kit. FFmpeg and ffprobe ship
beside the application; installed builds never search `PATH` for media tools.
The 800 × 720 Home window has a full dashed drop area with a centered open button and up to three recent-video thumbnails.

## Run the Linux build

Extract `dist/slicer-linux-x86_64-native.tar.gz`, keep the whole folder together, and run
`bin/slicer` inside it. Choose your recordings folder in **Settings**.

## Run and build on macOS

Open `dist/slicer-macos-aarch64/Slicer.app` on Apple Silicon, or drag it from
`dist/slicer-macos-aarch64.dmg` to Applications. Intel builds use `macos-x86_64`.
The app includes its icon, FFmpeg/ffprobe, and the private libmpv dependency closure.
No Homebrew installation is required on the destination Mac.

For development, select the media runtimes explicitly:

```sh
SLICER_FFMPEG_DIR="$PWD/build/ffmpeg/macos-aarch64/bin" \
SLICER_MPV_LIBRARY=/opt/homebrew/lib/libmpv.2.dylib cargo run --locked
```

Build the full desktop app and DMG on the matching architecture:

```sh
scripts/build-macos.sh --libmpv /opt/homebrew/lib/libmpv.2.dylib --development
scripts/smoke-macos-bundle.sh dist/slicer-macos-aarch64/Slicer.app
```

`--development` permits a local playback bundle without all corresponding source
archives. Source-complete releases omit that flag and provide the exact source
materials described in [packaging instructions](docs/packaging.md).
Ad hoc signing needs no Apple account. Downloads on other Macs still require
manual approval in macOS Privacy & Security. For distribution under default
Gatekeeper settings, pass `--sign-identity` and `--notary-profile`; the script
signs, notarizes, and staples both the app and DMG. Removing quarantine metadata
on the build machine cannot prevent macOS adding it when someone downloads a copy.

macOS playback uses the same libmpv engine through its OpenGL render API in an
input-transparent AppKit view, with VideoToolbox decoding and software fallback.
Exports use the separate bundled FFmpeg tool pair, with H.264/AAC video through the same OpenH264 encoder on all platforms. The native window supplies its
own corners and title bar; Slicer adds no second rounded window border.

## Build and run

Rust stable and GPUI's Linux development libraries are required for building
(X11/Wayland, xkbcommon, fontconfig, Vulkan graphics support). End users receive
the built application and bundled media tools.

```sh
cargo build --locked
# Development only: explicitly select media tools rather than silently using PATH.
SLICER_FFMPEG_DIR=/absolute/path/to/ffmpeg/bin \
SLICER_MPV_LIBRARY=/absolute/path/to/libmpv.so.2 cargo run --locked
```

See [packaging instructions](docs/packaging.md) for the pinned FFmpeg source build,
Linux distribution layout, source and notices, and other platform preparation.

To build the production Linux installer, run `scripts/package-linux-deb.sh`.
It writes an installable `slicer_<version>_<architecture>.deb` containing the
runtime, bundled FFmpeg/libmpv, icon, and compact license notices. Large source
archives are kept out of the installer; the portable packager can include them
when a source-complete compliance bundle is required.

When `build/playback-minimal/linux-<arch>` is present, the Debian packager uses
the reduced source-built libmpv profile automatically. This keeps the runtime
focused on local playback and GPU presentation instead of carrying mpv's
optional player, streaming, DVD, scripting, and alternate backend features.

## Workflow

Home checks the public project's raw `version.json` once on launch, outside the
UI thread. When a newer stable GitHub release has an uploaded asset for your
operating system and CPU, Home shows its version and a **Download** button.
The button opens that release's DMG on macOS or portable archive on Linux.
Offline checks do not interrupt editing. Settings shows the installed version.

## Publishing releases

The [release workflow](.github/workflows/release.yml) runs only when a GitHub
release is published, never on pushes. It builds Linux x86_64/ARM64 and macOS
Intel/Apple Silicon, tests the bundled runtimes, and uploads DMGs, portable
archives, Debian packages and SHA-256 checksums after every build succeeds.
macOS builds run on macOS 15; Ubuntu builds run on Ubuntu 24.04 (X11/XWayland).
The exact minimum macOS requirement is recorded from the bundled binaries.

For each stable release, set the same version in `Cargo.toml` and `version.json`,
update `Cargo.lock`, and commit those changes to `main`. Publish a GitHub release
tagged `v` plus that version, for example `v0.1.0`, from that commit. Release
validation rejects mismatched tags. A failed run can be rerun from Actions; it
replaces assets for the same release. Prereleases do not advertise app updates.
The repository and releases must be public for anonymous update checks.

Builds need no Apple subscription and use ad hoc signatures. The DMG includes
[installation instructions](packaging/MACOS-INSTALL.txt) for approving Slicer
on another Mac. To avoid that one-time approval, Developer ID signing and
notarization are supported by the macOS scripts.

## Editing videos

Choose a video folder in Settings. Home shows the three most recently modified
videos as cards with first-frame images; click a card to open an edit. The folder
choice persists across launches. Home refreshes every five seconds while open.
You can also open a video directly.

The editor selects the whole video initially. Drag the timeline handles to trim,
click or drag the playhead to scrub inside the selected cut. Play/Pause stays
centered below the preview, with mute on the right. Previous/next and Left/Right
jump to the cut boundaries; Space toggles playback. The cursor animates between
media-clock updates while playing. Audio files with a sound track display a
peak waveform on the timeline; it is extracted in a cancellable background
worker and never blocks playback or scrubbing.
Crop opens an adjustable rectangle in the editor preview, with corner and side
handles. Original, 1:1, 16:9, and 9:16 presets appear beside the Crop button.
Hold Shift while resizing to keep the current aspect ratio; the checkmark applies
the crop and X cancels.
The top-right Export button uses the saved defaults; its chevron opens Customize
export for the file name, location, format, and quality. MP4, MKV, WAV, and GIF
are available in the bundled export profile, and quality defaults to 100%.
Background jobs report progress and support cancellation. Existing files are never replaced. Failed and cancelled
exports remove their temporary output.

- **Fast cut:** copies compressed streams. Boundaries may move to nearby keyframes.
- **Exact cut:** decodes and encodes to place the boundaries precisely; takes longer.
- Exact video exports use the bundled OpenH264 encoder and AAC on Linux, Windows, and macOS. Quality scales the source bitrate budget; 100% does not request unbounded quality. MP4 metadata comes first for embedded playback. WAV uses PCM.
  Fast cuts keep the source codecs when the destination container supports them.
- Linux preview uses a persistent libmpv player in a native GPU-rendered surface,
  with audio, source-rate playback, and coalesced seeking. Hardware decoding is
  selected when supported; software decoding remains a fallback. On Linux,
  libmpv presents through OpenGL/EGL by default so it does not create a second
  Vulkan device alongside GPUI; set `SLICER_MPV_GPU_API=vulkan` only when that
  tradeoff is intentional.
- Home does not allocate the native video GPU context until a file is opened.
  Set `SLICER_PREWARM=1` to trade higher idle memory for the shortest possible
  first-open latency.
- Wayland desktops use XWayland for this embedded surface. Native Wayland and
  Windows video surfaces require separate integration and validation.
- Batch exports, hardware encoding, and multiple cut segments are deferred.

## Command-line verification

The same executable provides diagnostic commands without initializing the UI:

```sh
slicer binaries
slicer inspect "input video.mp4"
slicer export "input video.mp4" "trimmed video.mp4" 1.5 8.0 exact
slicer preview "input video.mp4" 2.5 frame.png
```

Export timestamps in the CLI are seconds. `cargo build --no-default-features`
builds only these diagnostic commands and the media engine.

```sh
cargo test --locked --no-default-features
SLICER_FFMPEG_DIR=/absolute/path/to/bundled/bin \
SLICER_TEST_FFMPEG_DIR=/absolute/path/to/bundled/bin \
  cargo test --locked --no-default-features -- --ignored
```

## Structure

- `src/ui.rs`: application state, initialization, window setup, and screen routing.
- `src/ui/home_view.rs`, `settings_view.rs`, `editor.rs`: individual screens.
- `src/ui/navigation.rs`, `preview_panel.rs`, `timeline.rs`, `export_controls.rs`: UI components.
- `src/ui/window_frame.rs`: rounded window surface and Linux window controls/resizing.
- `src/ui/actions.rs`: user actions and native dialogs.
- `src/ui/workers.rs`: background polling, library scans, and thumbnail coordination.
- `src/ui/theme.rs`, `formatting.rs`: shared styling and formatting helpers.
- `src/media.rs`: explicit bundled tool resolution and ffprobe JSON inspection.
- `src/job.rs`: argument construction, progress, safe publication, cancellation.
- `src/preview.rs`: bounded background still-frame decoding and seeking.
- `src/native_player.rs`: persistent native video/audio player and diagnostics.
- `src/ui/native_surface.rs`, `native_preview.rs`: native drawable and editor coordination.
- `resources/icons/hicolor/256x256/apps/slicer.png`: the application icon used
  by the native X11 window and Linux launcher package.
- `scripts/`, `packaging/`: reproducible media build and distribution tooling.

The UI modules share the root `SlicerApp` state so subscriptions and running jobs
keep a single owner. Components render that state and dispatch actions; media
work stays outside rendering.

Application code is MIT licensed. The Linux distribution also includes a
GPL-bearing libmpv runtime, FFmpeg, and Rust dependencies under their respective
licenses. See the distribution notices and corresponding source materials.

See `docs/native-video.md` for the renderer boundary and acceptance checks.
