# Multitrack editor: libmpv and OpenGL

## Run

```sh
LIBRARY_PATH="$PWD/build/native" cargo run --locked -- studio
cargo run --locked -- studio /absolute/project.slicer
cargo run --locked -- project-export /absolute/project.slicer /absolute/output.mp4
```

Set `SLICER_MPV_LIBRARY` to the bundled libmpv shared library and
`SLICER_FFMPEG_DIR` to the bundled tools directory when running a development build.
The original single-file trimmer remains available and uses its original output.

## Preview architecture

The timeline determines active clips and maps project time to each source time.
Playback and precise release frames use a persistent libmpv controller per active video, reusing the existing worker,
async commands, and coalesced seeks from the original player. A separate render
thread owns one EGL/OpenGL context and the libmpv render contexts. Each video
renders directly to an OpenGL framebuffer texture at its canvas footprint. No
video frame is read back to the CPU for preview. Image files decode once and
remain as GPU textures.

One OpenGL compositor applies placement, scale, rotation, opacity, layer order,
and selection outlines. libmpv video output is configured to SDR sRGB; the
compositor linearizes textures, blends premultiplied alpha into an RGBA16F target,
and converts the result back to sRGB for display. A native input-transparent X11
child presents the canvas; GPUI receives the mouse input. WGPU continues to draw
GPUI's interface but does not process video. The experimental WGPU compositor and
its custom GPUI surface patches have been removed.

OpenGL objects and libmpv render contexts are destroyed on their owner thread,
before player workers shut down and before the UI destroys the native surface.
All ordinary libmpv commands stay on the controller workers. Advanced render
control is deliberately disabled: the render owner can create and destroy other
sessions, and cannot promise the stronger deadlock constraints required for it.
Up to four inactive video sessions are retained across cuts. Inactive image
textures have a separate 256 MiB budget, so image collages are not evicted by
the video-session count limit. While idle, the two nearest
inactive clips are prepared one at a time at their entry/exit source timestamps.
Retained render contexts continue to receive render updates so pending libmpv
work does not stall while the clip is offscreen.

## Transport

- Play/pause changes playback state without seeking. Explicit scrubs, frame steps,
  and timeline edits advance the seek generation.
- While dragging, one coordinated target is processed while a replaceable slot
  retains the newest pending target. Completed samples can be displayed during
  the drag. An exact release supersedes pending drag samples.
- Precise release/playback compositions wait for all visible sources; approximate
  drag previews do not hold the entire canvas behind one slow decoder.
- Normal playback does not periodically seek to correct drift. Video rate is
  adjusted within 0.95–1.05 against the project clock. This is a bounded drift
  correction, not a guarantee of sample-exact synchronization between players.
- A separate FFmpeg audio worker mixes stereo float samples into one PulseAudio
  output. Audio output latency feeds the project clock. The UI never holds a
  transport lock across a blocking audio flush; audio decoders survive toggles.
- Projects without audio use the monotonic project clock. Switching to an audio
  clock cannot move the displayed playhead backward.
- Unchanged paused requests do not wake the render worker. Unchanged textures and
  transforms do not redraw the canvas.

## Editing and export

Import video, images, and audio. Imports use empty/new tracks at the playhead.
Later tracks overlay earlier tracks; audio adds together. Move clips across tracks,
trim edges, split, duplicate, hide/mute/lock tracks, and undo/redo. Drag canvas
objects to move them; Shift-drag scales. The inspector changes rotation, scale,
opacity, gain, and centering. Space toggles playback, arrows step a frame, B splits,
Delete removes, Ctrl-Z/Ctrl-Shift-Z undo/redo, Ctrl-S saves, Ctrl-O opens a project.

Projects are versioned JSON with nondestructive source references. Autosave writes
`<project>.autosave.slicer` or a recovery file under `$XDG_STATE_HOME/slicer`.
Autosave never overwrites a manual project save.

MP4 export uses the same libmpv/OpenGL composition with an offscreen EGL surface.
It waits for each exact frame, reads back the completed frame for CPU encoding,
and mixes audio at sample positions. Readback is confined to export. The current
encoder is MPEG-4 video/AAC audio. Existing output files are never replaced;
cancellation cleans up the temporary output. Export currently needs an X display.

## Build and tests

Linux development builds need EGL/OpenGL/X11 headers and runtime libraries,
FFmpeg 8 headers/libraries (libavcodec 62), and a libmpv build with the OpenGL
render API. `SLICER_AV_INCLUDE_DIR` and `SLICER_AV_LIB_DIR` override discovery.
The NVDEC rebuild described below was verified on the RTX 5080: libmpv reports
`hwdec-current=nvdec`. Older minimal bundles have CUDA/NVDEC compiled out.
Hardware support must be checked from the running decoder, not inferred from
the `hwdec` option or GPU presentation.

Portable packages must supply a `SLICER_ENGINE_BUNDLE` with FFmpeg library closure,
`ENGINE-NOTICE.txt`, and corresponding `source/` materials. Package scripts refuse
incomplete engine bundles. No new portable release has been produced.

```sh
LIBRARY_PATH="$PWD/build/native" cargo test --locked --lib
# Use full FFmpeg for raw-video fixture generation; run with access to X11/GPU:
SLICER_FFMPEG_DIR=/usr/bin SLICER_MPV_LIBRARY=/absolute/libmpv.so \
  LIBRARY_PATH="$PWD/build/native" \
  cargo test --locked --test editor_engine -- --ignored --nocapture
cargo run --locked --example editor_engine_check -- /absolute/video.mp4
SLICER_BENCH_PLAYBACK=1 cargo run --locked --example editor_engine_check -- /absolute/video.mp4
```

Integration tests cover two independently timed videos plus an image, backward
scrubs, transparent-image linear blending, rapid scrub reversals and final release,
play/pause without decoder restarts, audio/export, cancellation, and output safety.

The 1080p60/GOP120 two-video fixture measured playback render calls at p95 1.27 ms
(max 2.15 ms), with no playback resync seeks. This excludes visible presentation.
An exact random-seek run measured p95 394 ms before the coordinated drag scheduler;
it does not establish scrub parity with the original single-video player. Do not
compare these numbers directly to the removed decoder-only benchmark, which
excluded libmpv rendering and completion checks.

## Remaining limits

This implementation targets Linux/X11. Windows/macOS native canvas backends are
not implemented. OpenGL alone does not make this window integration portable.
Hardware decoding depends on the libmpv bundle and driver. More active sources
still cost more decoding work. Neighbor preloading removes session startup at
warmed boundaries, but arbitrary uncached long-GOP seeks still require decoding.
HDR-to-SDR previews are covered by the GPU/proxy checks below; native HDR/ICC
and wide-gamut output are not validated. Transitions and keyframe animation remain
outside this implementation. Audio output uses PulseAudio's blocking simple API
on its own thread; an unresponsive audio server can still delay shutdown.

Boundary regression checks also cover separated clips, idle preloading without a
second seek, retaining the previous complete canvas during a playing source change,
and intentionally rendering black in actual timeline gaps. On the user's staggered
1080p fixture, a warmed first-clip entry measured 0.1 ms (render-call completion).
Other arbitrary jumps still took roughly 70–180 ms; this is not a claim of instant
seeking. `SLICER_BENCH_PREWARM=1 cargo run --example clip_switch_check -- PROJECT`
exercises starts and midpoints in both directions.

## Scrub cache and proxies

The UI preview enables a dedicated `ScrubCache`; export renderers never enable
it. The frame worker keeps at most 240 scaled RGBA frames / 128 MiB, plus up to
four decoder sessions (each has its existing 8-frame / 32 MiB cache). Cache hits
and texture uploads run without decoding on the UI or OpenGL threads. Nearest
frames within 500 ms may be shown during dragging. Cold misses decode originals
on the worker until a proxy becomes available. Uncached long-GOP seeks can still
be slow while the proxy is being prepared.

A separate worker builds one proxy at a time: video-only, at most 640 pixels on
the longest edge, 30 fps, lossless PNG in MOV with every frame independently decodable. Proxies
are prepared automatically for imported and timeline media, before the playhead
reaches each source. Preparation diagnostics remain internal (there is no status strip). Two decoder/encoder threads and one filter
thread bound background transcode concurrency. Originals, project paths, audio,
playback, precise release and export are unchanged.

Proxies live under `$XDG_CACHE_HOME/slicer/scrub-v1` (otherwise `~/.cache`), or
`SLICER_PROXY_DIR`. Keys include the recipe, canonical source path, size and mtime.
Completed proxy files are pruned to 2 GiB, with a 512 MiB per-file limit. Publication
uses a temporary file and rename; shutdown kills/reaps the transcode and removes
its partial output. Failed or damaged proxies fall back to original media.
HDR uses the same libmpv GPU tone mapping as playback, with sequential
[frame stepping](https://mpv.io/manual/stable/#command-interface-frame-step)
in a separate renderer. Uncompressed PNG frames stream into the bundled native
PNG encoder, avoiding expensive Rust debug-build compression. Rotated sources
use libmpv until ffmpeg has prepared an oriented proxy.

```sh
SLICER_FFMPEG_DIR="$PWD/build/ffmpeg/linux-x86_64/bin" \
  LIBRARY_PATH="$PWD/build/native" cargo test --test scrub_cache -- --ignored
SLICER_PROXY_DIR=/tmp/slicer-proxy-check \
  cargo run --example scrub_check -- /absolute/video.mp4
```

On `drew.mp4` (720x1280 H.264, 30 fps, 10.63 seconds), 100 randomized single-layer
original seeks measured p50 127.59 ms / p95 339.58 ms. The proxy-backed scrub path
measured p50 1.48 ms / p95 2.23 ms on its first pass and 1.12 / 1.46 ms on a repeat.
With two overlapping layers and the NVDEC runtime, the first pass was 1.68 / 5.96 ms;
repeat was 1.18 / 1.96 ms. These measure request-to-completed-render, **exclude
proxy preparation, UI delivery and screen presentation**, and are not a guarantee
for every file. Exact release in that check took approximately 101 ms.

The saved 17-clip project exposed image-cache thrashing: crossing the shared
image end boundary measured p95 371–394 ms even after the video cache change.
With the separate image budget, the same project measured p50 1.34 / p95 1.99 ms
on its first pass and 1.02 / 1.90 ms on repeat; precise release took 55 ms.
This uses the same render-only measurement boundary above.

## Rebuilding NVIDIA playback support

The cached source profile is in `packaging/playback-nvdec.json`. Download
[nv-codec-headers 13.0.19.0](https://github.com/FFmpeg/nv-codec-headers/releases/tag/n13.0.19.0)
to `build/mpv-toolchain/src/nv-codec-headers-13.0.19.0.tar.gz`. Then run:

```sh
python3 scripts/build-playback-nvdec.py
```

This uses the existing `build/mpv-toolchain/src/{ffmpeg-7.1.5,mpv-0.41.0}` source
trees and dependency sysroot. The header archive is SHA-256 checked; CUDA decode
and GL interop are required rather than silently disabled. Static FFmpeg symbols
are hidden to prevent ABI interposition with the engine's FFmpeg 8 libraries.
The script leaves its library in `build/mpv-toolchain/nvdec/mpv-build`; it does not
replace the bundled runtime. Package it using the playback bundler (which sets
`$ORIGIN` runtime paths) and retain the source materials and notices. No NVIDIA
driver libraries are bundled; those come from the host.

## Visual and pause regression fixes

The original MPEG-4 proxy recipe was found to encode green/magenta striped
frames with the bundled FFmpeg on the portrait H.264 recording. Proxies now use
lossless PNG/MOV, and the recipe key changed so old corrupt files are never
reused. The cache pixel test compares decoded proxy colors against a clean
lossless source; `scrub_visual_check` saves and compares actual GPU output.
PNG proxies take more disk space; the existing cache limits still apply. The
previous MPEG-4 performance figures above describe the former recipe, not the
new PNG recipe.

Pause closes the PulseAudio stream and resume creates a freshly buffered stream.
Flushing an unbuffered stream kept its underrun clock running during a pause,
which reproduced a roughly one-second position jump after a one-second pause.
The studio transport test exercises repeated pauses against real PulseAudio.
Selecting a timeline clip now also pauses audio immediately. The multitrack
timeline shares the original trimmer's handle/playhead painters and neutral
palette, with centered round transport controls.


## Workspace panels and media library

The editor uses Files, Preview, Properties, and Timeline panels. Shared dividers
occupy exactly 5 logical pixels; drag either vertical divider to resize the side
panels or the horizontal divider to resize the timeline. There is no bottom
status strip. Properties follow the selected timeline clip and edits respect
track locking and undo history.

Imports and external file drops populate Files. Drag an item from Files onto an
unlocked timeline track to create an independent clip at that position. The media
library is saved with the project; older projects populate it from their clips.

Event-level panel, drag/drop, and property-control checks run with:

```sh
LIBRARY_PATH="$PWD/build/native" cargo test --locked --features ui-tests --bin slicer panels_resize_and_media
```


## Stable filmstrips and HDR media

Filmstrip sampling uses a source-time grid. Cuts and trims clip the existing
samples instead of moving the sampling origin and rebuilding the strip. Tile
width comes from the media bin source aspect, independently of the timeline
instance transform, so decoded frames cannot rearrange the strip while it fills. Completed frames remain cached across edits, and failed reads
retry instead of becoming permanently blank. Videos without audio have a 49px
thumbnail area in the same 66px clip height; audio videos use 31px thumbnails
plus their 18px waveform strip.

The thumbnail worker uses finished scrub proxies when available. Before then,
HLG/PQ or oriented media uses a private GPU renderer; ordinary SDR sources use
the software decoder. Worker cancellation and GPU teardown finish before the
thumbnail service is destroyed.

On the user's 3840×2160/60fps HLG H.264 clip (28.77 seconds), four cold thumbnail
samples completed in 1.14 seconds. Proxy preparation took 37.27 seconds. Warm
random scrub-to-render measured p50 8.06ms / p95 9.23ms / p99 9.52ms, excluding
presentation. Release to original NVDEC playback took 245ms. HDR proxy samples
matched the GPU playback reference with mean channel error 0–1.54/255.

GPU regression checks (set the bundled FFmpeg and mpv paths as above):

```sh
SLICER_THUMBNAIL_TEST_FILE=/absolute/hdr-video.mp4 \
  LIBRARY_PATH="$PWD/build/native" cargo test --bin slicer hdr_filmstrip -- --ignored --nocapture
SLICER_HDR_TEST_FILE=/absolute/hdr-video.mp4 \
  LIBRARY_PATH="$PWD/build/native" cargo test --test scrub_cache hdr_proxy -- --ignored --nocapture
```

## Text, backgrounds, output frame, and direct editing

The timeline toolbar adds editable text (T) and color backgrounds. Text supports
multiline Unicode, installed fonts, size, bold/italic, alignment, foreground and
box colors with alpha. Text content updates live; Apply properties commits font,
color, timing, and fade fields. Generated clips can be moved, trimmed, split,
duplicated, copied, and transformed just like media. Their styling lives in the
project, and preview/export use the same cosmic-text rasterization and GPU
compositor. Font size is relative to a 1080-pixel-high output and scales with
output resolution. Text adds an overlay track; backgrounds add a bottom layer.

Canvas in Properties (also opened by the crop icon beside Preview) offers 16:9,
9:16, square, 4:5, and 4K presets plus custom even dimensions from 2 to 8192 and
frame rates from 1 to 240. The outlined black preview frame is the output area;
the gray surround is excluded. Changing aspect preserves media proportions.
Fit/Fill controls place selected media within that frame. Newly dropped media
fits the current canvas rather than inheriting the old 16:9 display shape.

| Input | Action |
| --- | --- |
| Mouse wheel / middle drag | Pan timeline |
| Ctrl+wheel / pinch | Zoom around pointer |
| Shift+wheel | Scroll tracks vertically |
| Trackpad horizontal / vertical | Pan timeline / scroll tracks |
| Ctrl+click / Shift+click | Toggle selection / select range on a track |
| Drag selection / clip edges | Move group / trim clip |
| Space / T / B | Play-pause / add text / split selected clips |
| Ctrl+C / X / V / D / A | Copy / cut / paste at playhead / duplicate / select all |
| Delete / Shift+Delete | Delete / ripple delete on affected tracks |
| Ctrl+Z / Ctrl+Shift+Z (or Ctrl+Y) | Undo / redo |
| Left / Right | Step one frame |
| Shift+Left / Right | Seek one second |
| Ctrl+Left / Right | Previous / next clip boundary |
| Alt+Left / Right | Nudge selection one frame (Shift: ten frames) |
| Home / End | Start / end |
| + / − / F | Zoom in / out / fit timeline |
| PageUp / PageDown | Pan by most of one viewport |
| N / Escape | Toggle snapping / clear selection |
| Ctrl+S / O / I | Save project / open project / import media |

Timeline shortcuts leave focused text fields alone. Locked tracks reject edits;
group moves preserve relative positions. The keyboard icon opens an in-app guide.

Validation covers typing without triggering playback, clipboard undo/redo,
pointer zoom, middle-button panning, vertical track scrolling, group dragging,
locked clip protection, and the preview aspect after custom canvas edits. The
GPU integration test compares an exported portrait title/background/fade sequence
against its preview and verifies that editing text while paused redraws it.

```sh
LIBRARY_PATH="$PWD/build/native" cargo test --locked --features ui-tests --bin slicer interaction_tests
# With bundled FFmpeg/mpv and a GPU session:
LIBRARY_PATH="$PWD/build/native" cargo test --test graphics_editor text_background -- --ignored --nocapture
```
