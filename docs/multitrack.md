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
Each active video has a persistent libmpv controller, reusing the existing worker,
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
Up to four inactive sources are retained across cuts. While idle, the two nearest
inactive clips are prepared one at a time at their entry/exit source timestamps.
Retained render contexts continue to receive render updates so pending libmpv
work does not stall while the clip is offscreen.

## Transport

- Play/pause changes playback state without seeking. Explicit scrubs, frame steps,
  and timeline edits advance the seek generation.
- While dragging, one coordinated target is processed while a replaceable slot
  retains the newest pending target. Completed samples can be displayed during
  the drag. An exact release supersedes pending drag samples.
- Compositions are published only after all visible videos have settled;
  the previous complete canvas stays visible while a new target is being decoded.
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
The minimal bundled libmpv reports software decoding on the validation machine;
GPU presentation must not be described as hardware decoding.

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
No proxy generation or broad decoded-frame cache is implemented. HDR/ICC and wide-gamut output are not validated; SDR is the tested
path. Waveforms, filmstrip thumbnails, transitions, and keyframe animation remain
outside this implementation. Audio output uses PulseAudio's blocking simple API
on its own thread; an unresponsive audio server can still delay shutdown.

Boundary regression checks also cover separated clips, idle preloading without a
second seek, retaining the previous complete canvas during a playing source change,
and intentionally rendering black in actual timeline gaps. On the user's staggered
1080p fixture, a warmed first-clip entry measured 0.1 ms (render-call completion).
Other arbitrary jumps still took roughly 70–180 ms; this is not a claim of instant
seeking. `SLICER_BENCH_PREWARM=1 cargo run --example clip_switch_check -- PROJECT`
exercises starts and midpoints in both directions.

The preview scheduler now coalesces drag samples only within the same visible
clip set. Crossing an overlap, cut, image edge, or gap immediately supersedes the
old sample; an obsolete scene cannot subsequently be presented. Project edits,
canvas size, selection, and transport changes also invalidate sample reuse.
Mouse release at the same target reuses the exact decoded frame: NativePlayer's
"exact" request flag affects pacing, while both drag and release issue exact mpv
seeks. A discarded composed frame marks the canvas dirty for the next presentation.

Regression coverage includes every boundary in the staggered image/two-video
arrangement in both directions, obsolete-video-to-image-only presentation, and
release without another seek. The 320x180 image-only presentation regression
measured about 1.1 ms worst over 12 transitions. The saved 1080p60/GOP120 staggered
project still measured approximately 15–143 ms for uncached exact video jumps;
those measurements exclude UI input and display scanout. This fix removes extra
scheduler latency; it does not eliminate the cost of decoding uncached frames.
