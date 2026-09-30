# Native video architecture

The Linux editor embeds a native X11 drawable inside the GPUI preview bounds.
libmpv owns video decoding, GPU presentation, audio output, and media timing.
GPUI owns navigation, the timeline, trim selection, transport, and export dialogs.
Wayland desktops run this window through XWayland when DISPLAY is available.
macOS embeds an input-transparent NSOpenGLView in GPUI's AppKit view. libmpv's
OpenGL render API draws directly into that CGL drawable; it never opens a
separate player window or routes playback frames through the GPUI image cache.
VideoToolbox is the macOS hardware-decoder preference, with software fallback.
The CGL context remains current for renderer creation, drawing, and destruction.
The render context is freed before the player, and a retained core keeps libmpv
alive until both the event worker and renderer have released it.
Native Wayland and Windows surfaces remain separate integrations.

## Responsibilities

- `src/native_player.rs`: persistent libmpv handle, commands, coalesced seeks,
  playback state, renderer/decoder diagnostics, and shutdown.
- `src/ui/native_surface.rs`, `native_surface_macos.rs`: child drawable, scale-aware placement, visibility,
  and destruction.
- `src/ui/native_preview.rs`: editor lifetime, loading, seek completion, and
  synchronization between the media engine and the controls.
- `src/preview.rs`: independent one-frame thumbnail and CLI preview generation.
- `src/job.rs`: isolated export jobs using the bundled export FFmpeg.

Live video must not be converted to PNGs or uploaded through GPUI's image cache.
The video renderer is a native embedded surface, not a shared texture in GPUI's
compositor. It must be hidden for modal overlays and while navigating away.
The player must detach before its child drawable or GPUI parent is destroyed.
Both the title-bar close action and OS close callback stop the player first.
Mouse movement also checks the physical button state, so releasing over the
native child cannot leave the timeline issuing seeks after a drag.

## Playback and seeking

Playback commands use libmpv's asynchronous command API, with errors collected
from command replies. Repeated pause/mute requests are deduplicated so scrubbing
does not block the event worker on a synchronous playback command.

Keep the player and renderer alive across pause/resume and seeks. Dragging sends
replaceable, paced accurate seeks, so long keyframe intervals do not limit the
preview to occasional keyframes. Mouse release sends an immediate exact seek. The timeline
keeps the requested position while that seek is pending, instead of snapping back
to an older playback observation. The selected range controls playback start/end;
preview mute affects playback only, never exported audio. Scrubbing is constrained
to that selection. Previous/next and Left/Right jump to its boundaries.

GPUI requests display-synchronized frames while playing. A small playhead clock
interpolates between libmpv timestamps and gently corrects clock jitter; pause and
seek reset it to the authoritative position. Interpolation is bounded to 150 ms
beyond the latest sample so a stalled decoder cannot leave the cursor running.

Hardware decoding and GPU presentation are distinct. Record the selected decoder,
video output, dimensions, frame rate, and audio state during acceptance testing.
A GPU-rendered software-decoded video is a valid fallback, not proof of hardware
decoding. A software GPU renderer on Xvfb is useful for integration tests, not
proof of native hardware performance.

On Linux, the default decoder preference is NVDEC, then VA-API, with software decoding as
the fallback. The reduced distribution intentionally does not bundle CUDA/NVDEC,
so software decoding is the normal result on that profile. Vulkan Video decoding
is excluded after sustained scrubbing stalled on the tested NVIDIA system; GPU
presentation is retained. Linux presentation uses libmpv's OpenGL/EGL path by
default, which avoids a second Vulkan device in the process while keeping the
video surface GPU-rendered. Set `SLICER_MPV_GPU_API=vulkan` to compare the
alternate path. `SLICER_MPV_HWDEC` is an explicit diagnostic override, not
required for normal launch.

## Acceptance checks

- Open H.264/AAC 1080p60 and inspect native renderer/decoder state.
- Play with audio; pause/resume without recreating the player.
- Repeatedly scrub a long-GOP clip; confirm final exact position and no PNG work.
- Drag both trim handles; confirm final selection and range playback.
- Resize/maximize/restore; confirm video bounds match GPUI at desktop scaling.
- Open/close Export; confirm video does not cover the modal.
- Navigate Home/Settings and reopen; confirm hidden audio/video stops.
- Export a clip, then inspect it with bundled ffprobe.
- Run the packaged app with development overrides removed.
- Close the app during playback; confirm no surviving player/decoder worker.

## Crop and export controls

Crop mode keeps the native paused frame visible while a still frame is decoded
in the background, then swaps only after the GPUI image has been painted. An
unchanged paused position reuses the cached image. Crop mode stays inside the editor preview,
with movable selection and corner/side resize handles. Presets and Apply/Cancel
appear only while cropping; normal playback remains on the native GPU surface. Applying a rectangle sets
mpv's `video-crop` presentation property in source pixels. Export uses the same
even-pixel rectangle through FFmpeg's crop filter and encodes precise cuts.
Rotation/display-matrix metadata is currently rejected for cropped exports to
avoid exporting a different region from the selected one. Audio-only exports
ignore the video crop.

The Export split button uses saved defaults; its chevron opens Customize export.
Settings persist format, 50–100 quality, output folder, and clipboard preference.
UI exports use precise encoding automatically; the CLI retains explicit modes.
Precise exports on every platform encode H.264 with OpenH264 and AAC, and MP4 metadata
is placed before media data for browser/Discord playback. Existing MPEG-4 Part 2
exports need to be exported again to receive the new codec. Quality scales a
source-derived bitrate budget: 100% uses the source video bitrate, while 50%
targets half that bitrate. AAC is capped at 192 kb/s per track. If stream rates
are absent, file size/duration supplies an average budget. This controls size;
100% does not promise lossless output or an exact file-size ratio.
Default exports choose a free suffixed filename, with atomic no-replace protection
still enforced by the export worker. Crop dimension probing runs on that worker.

On Linux, clipboard copying owns an X11 selection with `text/uri-list` and
`x-special/gnome-copied-files`, so file managers receive a file rather than plain
path text. The app retains ownership until another copy or app exit. Successful
export closes customization and shows a top-center toast with Open folder.
On macOS, NSPasteboard receives an NSURL for the complete exported file via
NSPasteboardWriting. No image or decoded first frame is written to the clipboard.

Resize batches configure/shape requests, keeps the surface mapped through empty
layout passes, and avoids an opaque background clear. The vendored GPUI WGPU
renderer also polls without waiting for all in-flight work during a drawable
resize, so the event loop can keep presenting frames while the window is being
dragged. GPU acceptance on the NVIDIA test system sampled 180 frames through
repeated window size changes with no black samples in the video region; other
compositors may behave differently.

## External file drops

Slicer captures external file drops at window level because GPUI's bundled X11
backend reports XDND coordinates in device pixels, which otherwise misses logical
hitboxes on scaled displays. Drag-over state shows a thicker white border and
“Drop video to open”; leaving the window clears it. The native video child has
an empty input shape, keeping GPUI responsible for pointer and drop targets while
libmpv still paints video. `examples/xdnd_check.rs` sends a real XDND file offer
on an isolated X11 display for acceptance testing at 200% scaling.

## End-of-playback state

Natural EOF and selected-range completion latch a paused state at the exact
selected end. Late player position and pause events cannot move that terminal
playhead. The next Play starts at the selected beginning. The timeline displays
the end boundary at the handle center, while explicit end-frame seeking still
uses a decodable position just before the exclusive end.

Run `native_player_check --end-check <video>` on an isolated X11 display to check
full-file EOF, replay, trimmed EOF, and trimmed replay against libmpv.

The pinned `vendor/gpui-pre-linux` patch requests `text/uri-list` before text
alternatives and delays drop submission until selection conversion completes.
It also scales X11 drag coordinates and filters unrelated selection notifications.
`xdnd_check` supports `mixed delayed type-list` to exercise those file-manager
cases; the old URI-only, six-second hover test missed both failures.

## Opening a video

Opening reads stream metadata and decodes the original media; it does not encode
a proxy or transcode the video. libmpv library/GPU initialization runs on a
joinable background thread in parallel with metadata inspection. The native
player persists across files, and a normal load does not request a redundant
seek back to zero. Closing joins initialization before destroying its X11 surface.

## macOS integration

The app bundle carries `Slicer.icns`, native file document associations, Open/Quit
menus and keyboard shortcuts. Finder open-file events feed the existing editor
workflow. File copying writes an NSURL to NSPasteboard, so Finder receives a file
instead of text. AppKit owns the window corners and title bar; Slicer paints an
opaque, square content area. Retina drawable dimensions use `convertRectToBacking`
while layout remains in logical points. The native view's `hitTest:` returns nil
so timeline events and external file drops continue to reach GPUI.

Use `scripts/build-macos.sh` for the desktop app and DMG, and
`scripts/smoke-macos-bundle.sh` to verify the installed tool and dylib layout.
