# Validation

Run all checks from the repository root. FFmpeg tests require explicit paths so
that successful tests cannot accidentally hide a missing media bundle.

```sh
cargo test --locked --no-default-features
SLICER_TEST_FFMPEG_DIR="$PWD/build/ffmpeg/linux-x86_64/bin" \
SLICER_FFMPEG_DIR="$PWD/build/ffmpeg/linux-x86_64/bin" \
  cargo test --locked --no-default-features -- --ignored
cargo check --locked
cargo fmt --check
```

`tests/write_failure.rs` injects an ENOSPC error and a partial write. It verifies
that the error reaches the caller, the original is unchanged, and neither a
finished nor a partial output remains. This simulates disk exhaustion without
filling a disk.

`tests/preview_integration.rs` creates a moving video with a Unicode filename and
checks that seeking yields two different PNG frames. The UI uses the same worker
for first-frame library thumbnails. Live editor playback uses libmpv.

## Desktop acceptance checklist

- Start with no saved folder: Home should offer folder setup and direct Open.
- Choose a folder containing more than three videos and unrelated files.
- Verify exactly the latest three videos, with first-frame cards.
- Click a card; verify preview, transport, timeline, and the full initial trim range.
- Drag both trim handles, scrub, play/pause, and skip forward/backward.
- Open Export; change the filename, location, format, cut mode, and quality.
- Return Home; create another video and check that it appears after refresh.
- Restart with the same config and verify the selected folder persists.
- Test an empty, missing, unreadable, and Unicode-named folder.
- Test direct Open and drag/drop.
- Test exact/fast trim, WAV extraction, destination selection and existing file
  refusal, progress, cancellation, and Open output folder.
- Close during export and confirm the worker stops and removes partial output.

Windows and macOS need native build and desktop acceptance runs before release.

## This Linux environment

The runtime `libxkbcommon-x11.so.0` is installed, but its development symlink
`libxkbcommon-x11.so` was absent. The build was completed without changing system
files by creating `build/native/libxkbcommon-x11.so` pointing at that runtime,
and passing `LIBRARY_PATH="$PWD/build/native"` to Cargo. A normal development
machine can instead install its distribution's `libxkbcommon-x11-dev` package.
The delivered executable uses the ordinary runtime library.

The native GUI was checked on an isolated Xvfb display. A card opened the editor,
a sample exported successfully through the UI, and Home then refreshed to show
the exported file as the newest card. A visible desktop launch also succeeded.

## Native editor playback

`tests/native_player_integration.rs` validates a persistent libmpv handle,
playback advancement, pause stability, rapid coalesced seeks, frame-accurate final
seeks, reload, range stopping, and bounded shutdown. Explicit runtime overrides
fail acceptance when the library cannot initialize.

```sh
SLICER_MPV_LIBRARY=/absolute/path/to/libmpv.so.2 \
  cargo test --locked --test native_player_integration
```

The integration fixture is `build/validation/native-library/Native 1080p60 日本.mp4`.
Without this fixture or a discoverable development runtime, native integration
tests report a skip. Release acceptance must provide both explicitly.

Linux acceptance on 2026-09-15:

- RTX 5080: the reduced libmpv runtime selected `gpu-next` through OpenGL/EGL
  for a 1920×1080 60 fps H.264 source; this profile fell back to software
  decoding because CUDA/NVDEC was intentionally not bundled.
- Isolated X11 display: first paused frame, trim dragging with exact final frame,
  independent scrubbing, export overlay hide/remap, and Settings surface hiding.
- UI export: selected 6.781 s clip produced a 6.802 s MP4 with MPEG-4 video at
  1920×1080/60 fps and AAC audio (normal frame/audio packet rounding).
- Resized the live editor from 800×720 to 1100×850; the same mpv child resized
  from 768×485 to 1068×615. Reopening from Home retained the same player/window.
- Native surface geometry tests cover fractional scaling and rounded clipping.
- `examples/native_player_check.rs` played the fixture on the RTX for two seconds,
  observed 60 fps/OpenGL/AAC, then landed exactly at 1.250 s and shut down cleanly.

The surface must be mapped and sized before libmpv initializes. Keeping it hidden
until the first decoded frame causes mpv's own child to remain unmapped.
See [native-video.md](native-video.md) for architecture and platform limitations.
