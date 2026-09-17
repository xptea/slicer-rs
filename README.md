# Slicer

Native Linux video trimming and layered composition toolbox built with GPUI Kit.
FFmpeg and ffprobe ship beside the application; installed builds never search
`PATH` for media tools.
The 800 × 720 Home window has a full dashed drop area with a centered open button and up to three recent-video thumbnails.

## Run the Linux build

Extract `dist/slicer-linux-x86_64-native.tar.gz`, keep the whole folder together, and run
`bin/slicer` inside it. Choose your recordings folder in **Settings**.

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

Layered projects are stored as `.slicer.json` files. Open a project and use
**Add media** (or drop several files into the editor) to keep video, image,
text, shape, and audio assets as independent tracks. The project model keeps
exact rational time ranges, source offsets, transforms, crop, opacity, layer
order, audio gain, and undo/redo state. `Save` persists paths relative to the
project when possible; missing paths are reported on reopen instead of silently
flattening or replacing a layer.

- **Fast cut:** copies compressed streams. Boundaries may move to nearby keyframes.
- **Exact cut:** decodes and encodes to place the boundaries precisely; takes longer.
- The initial native FFmpeg profile encodes MPEG-4/AAC in MP4 or MKV, and PCM WAV.
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
  other-platform video surfaces require separate integration and validation.
- The layered export path renders the project graph at the requested output
  resolution. The native preview remains the compatibility fast path for a
  single video; full multi-layer live preview and hardware interoperability are
  still platform-gated.

## Command-line verification

The same executable provides diagnostic commands without initializing the UI:

```sh
slicer binaries
slicer inspect "input video.mp4"
slicer project create "cut.slicer.json" 1920 1080
slicer project add "cut.slicer.json" "video.mp4" 0
slicer project add "cut.slicer.json" "overlay.png" 0
slicer project relink "cut.slicer.json" 1 "moved-video.mp4"
slicer project inspect "cut.slicer.json"
slicer project render "cut.slicer.json" "composite.mkv"
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
- `src/project/`: rational-time layered project model, scene evaluation,
  commands, storage, and the single-video compatibility adapter.
- `src/composition/`: deterministic CPU reference compositor for RGBA, images,
  shapes, crop, transforms, opacity, and text fallback.
- `src/export.rs`: rational frame scheduling, composed MP4/MKV/GIF/WAV export,
  cancellation, and no-overwrite publication.
- `src/session.rs`: model-authoritative import/save/reopen/recovery session.
- `src/engine/`: decoder fallback, playback contracts, clock, scheduler, audio
  mixer, cache, and GPU lifecycle contracts.
- `src/ui/layered_timeline.rs`, `canvas_tools.rs`, and `layer_inspector.rs`:
  model-facing timeline and canvas interaction contracts (the legacy surface
  remains the compatibility UI until the direct compositor route is validated).
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
