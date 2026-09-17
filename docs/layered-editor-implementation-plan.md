# Layered editor: end-to-end implementation plan for future subagents

Status: implementation authorized by the user on 2026-09-16. This document remains
the target plan; the current repository contains a validated vertical slice and
records the platform-gated work that is not yet claimable as complete.

## 1. Objective and completion boundary

Extend Slicer from a single-video trimmer into a Linux layered editor that can
play, arrange, save, reopen, and export multiple simultaneous videos, text,
images, shapes, and audio. Preserve responsive opening, playback, scrubbing, and
resizing. Build a direct Vulkan compositor and an application-controlled FFmpeg
decode/scheduling pipeline. Keep the existing libmpv player available during
development and for supported simple projects after rollout.

Direct Vulkan is the chosen engineering direction, not an established speed
advantage. Performance claims require measurements against the existing player
on the same machine and media. Adobe-level performance is an aspiration, not an
acceptance criterion or a promised outcome.

The first complete release includes:

- Multiple video, still-image, text, solid rectangle, and audio clips.
- Canvas dimensions, rational frame rate, background color, and output duration.
- Timeline placement, source trimming, splitting, duplication, deletion, layer
  ordering, visibility, and locking. Overlapping visual clips have explicit order.
- Position, scale, crop, rotation, and opacity with direct canvas manipulation.
- Text editing, font selection, size, alignment, wrapping, and color.
- Per-clip audio gain/mute, synchronized audio mixing, and a separate monitor mute.
- Project save/load, missing-media relinking, autosave recovery, undo/redo.
- Deterministic full-resolution export, including MP4/MKV, GIF, and WAV behavior
  consistent with the formats currently offered by the UI.
- Bounded caches, optional proxies, diagnostics, packaged runtime, and regression tests.

Initial platform: Linux x86_64, X11 and XWayland, matching today's deployment.
Native Wayland, Windows/macOS, HDR output, arbitrary effects/plugins, transitions,
keyframe animation, speed ramps, reverse playback, nesting, and collaboration are
separate follow-up work. Handle HDR inputs explicitly: implement and test SDR
conversion or reject them descriptively; do not silently interpret them as SDR.

## 2. Current repository constraints

Read these files before editing their subsystems:

| Existing area | Files | Migration approach |
| --- | --- | --- |
| Singleton editor state | `src/ui.rs`, `src/ui/actions.rs` | Introduce a model-authoritative editor session behind the existing screens |
| Single-video playback | `src/native_player.rs`, `src/ui/native_preview.rs` | Retain as legacy backend; implement a new composition backend |
| X11 presentation | `src/ui/native_surface.rs`, `src/ui/preview_panel.rs` | Reuse geometry/lifetime behavior where applicable |
| Trim UI and crop | `src/ui/timeline.rs`, `src/ui/crop.rs`, `src/ui/playhead_clock.rs` | Adapt to project time and selected clips; preserve interaction behavior |
| Media and assets | `src/media.rs`, `src/preview.rs`, `src/waveform.rs` | Reuse inspection and thumbnails; key workers by asset and generation |
| Export | `src/job.rs`, `src/ui/export_controls.rs` | Preserve job lifecycle and atomic publication; add composition jobs |
| GPU/UI | `vendor/gpui-pre-wgpu`, `vendor/gpui-pre-linux` | Minimal, isolated changes only after a presentation decision |
| Distribution | `scripts/`, `packaging/`, `docs/packaging.md` | Add reproducible decoder/encoder library runtime |
| Verification | `tests/`, `examples/native_player_check.rs`, `docs/validation.md` | Retain old checks; add composition and real-GPU performance checks |

Important findings from the current checkout:

- The repository has a committed baseline (`814d8d6`, `main`) and the layered
  implementation is currently an intentional uncommitted worktree change.
  Preserve unrelated edits and do not reset/clean the worktree. Future
  contributors should use explicit file ownership and review the complete diff
  before committing or branching.
- The editor owns one path, one media record, one waveform, and one native player.
  Trim values are currently read from text inputs. Model state must become the
  source of truth before adding tracks.
- The player uses an X11 child window. Its contents are outside GPUI's compositor.
  Normal GPUI widgets cannot be assumed to overlay that window.
- GPUI uses wgpu. The current player prefers OpenGL to avoid a second Vulkan
  device's overhead. Earlier Vulkan Video decoding stalled during seeking on the
  tested NVIDIA setup. Neither fact proves a Vulkan compositor will be slow.
- The documented reduced playback bundle used software decoding on RTX 5080;
  CUDA/NVDEC was intentionally not bundled. GPU presentation is not proof of GPU
  decoding.
- `scripts/build-ffmpeg.sh` builds static libraries/CLI, disables autodetection
  and x86 assembly, and has a restricted codec/filter profile. Existing binaries
  are not a drop-in SDK for the new engine. The locked source is FFmpeg 7.1.5;
  verify compatibility rather than silently upgrading it.

## 3. Architecture and decisions that must be proven

```text
Project + edit commands + undo history
                  |
          immutable project snapshot
                  |
       timeline evaluator / scene plan
          /                       \
 preview scheduler             offline frame iterator
     |                              |
 asset decoders + frame cache + audio mixer
     |                              |
 Vulkan compositor: common transforms, text, color, alpha
     |                              |
 preview presentation          encoder / muxer
                                    |
                             safe export publication
```

### 3.1 Presentation decision gate

Prototype these two routes before implementing the full editor:

1. An owned Vulkan device/swapchain presenting into the existing native child.
   This is the initial candidate because it isolates the compositor from GPUI.
   The compositor also draws selection outlines, handles, and guides. GPUI
   captures input and draws surrounding controls; native-surface hiding for
   modals remains necessary.
2. An offscreen Vulkan compositor whose result is imported into GPUI's renderer.
   Investigate device sharing or external image/semaphore interoperability using
   the pinned wgpu implementation. Do not assume an arbitrary Vulkan device or
   image can be handed to wgpu. Prove ownership, queue access, layout transitions,
   synchronization, adapter matching, and teardown.

Deliver a decision record with measured memory/latency and maintainability
tradeoffs. Select one production presentation route. Do not maintain two new
presentation implementations indefinitely. Prefer integration if it is proven
and competitive; choose the child surface if integration remains unsafe or
unproven. If neither meets the one-video gate, fix the spike before advancing.
Changing the primary direction to wgpu requires a documented revised decision,
not an unannounced substitution by an implementation subagent.

### 3.2 Decoding and hardware sharing

- Use FFmpeg libraries in-process with pinned bindings and an explicit runtime.
  Keep FFI and resource ownership in a narrow module.
- Implement software decode plus reusable YUV upload buffers first as a correct
  fallback. Perform conversion/scaling on the GPU where supported.
- On the reference NVIDIA machine, probe NVDEC/CUDA frame interoperability with
  the chosen Vulkan device. On additional certified Linux devices, implement
  an appropriate path such as VA-API plus supported external-memory import.
  These are separate capabilities; do not advertise either before testing.
- Query actual format, modifier, allocation, and synchronization support. Fall
  back when import is unsupported. Record decoder and transfer path separately.
- A GPU-to-GPU copy may be acceptable. CPU readback/upload must be visible in
  diagnostics; do not call that path zero-copy. Vulkan Video is not required.
- Hardware contexts, frame pools, queues, and decoder instances are bounded and
  reused. Enable assembly optimizations in the supported CPU build if the build
  toolchain permits; measure the effect and retain a reproducible fallback.

### 3.3 Timing, threading, and ownership

- One engine owner accepts asynchronous commands. The UI never waits for decode,
  filesystem I/O, GPU idle, shader compilation, or a global engine lock.
- Project edits produce immutable revisions; render work tags its revision and
  seek generation. Stale work cannot overwrite a newer seek or edited frame.
- Demux/decode workers feed bounded queues. The render owner submits GPU work;
  a retirement queue releases frame references only after GPU completion.
- Audio output uses a prefilled ring buffer; its callback must not allocate,
  acquire contended locks, perform I/O, or decode.
- During playback with an audio device, derive presentation time from consumed
  audio samples accounting for device latency. Use a monotonic clock for silent
  playback. Define pause, seek, device-loss, and clock-switch behavior explicitly.
- During scrubbing, the requested timeline time owns the UI playhead. Coalesce
  seeks, discard stale frames, and resolve an accurate frame after pointer release.
- Offline export uses exact frame/sample timestamps and never drops frames.
  Interactive preview may drop late video frames but must not drift silently.
- Shutdown order: cancel producers, stop audio, retire GPU work safely, release
  imported frames, destroy decoders/render resources, then destroy the surface.

## 4. Shared contracts to freeze before parallel implementation

These are semantic requirements, not final Rust signatures. The coordinator
owns `src/project/`, `src/engine/api.rs`, and contract changes until interfaces
are stable; assign implementation ownership explicitly afterward.

### Project model

- Versioned `Project`: ID, asset registry, tracks/clips, canvas, rational frame
  rate, work/export range, explicit duration policy, and background.
- Stable `AssetId`, `TrackId`, `ClipId`. Assets reference originals plus optional
  proxy/cache metadata. Cached results are not serialized as authoritative media.
- Rational/integer time with checked arithmetic. Preserve source stream time
  bases and VFR timestamps. Float seconds are only UI/API boundary conversions.
- Half-open clip intervals `[start, end)`. Initial playback speed is 1. Source
  time is `source_in + (project_time - clip_start)`; select frames using their
  presentation intervals, not by assuming every source has the project FPS.
- One clip kind per video/image/text/shape/audio. Define video-with-audio linkage
  so importing a video does not accidentally play its audio twice.
- Explicit visual ordering for all overlaps. Tracks group clips; track and clip
  order determine a documented total stacking order.
- Canonical canvas-pixel coordinates, anchor, position, scale, rotation, source
  crop, opacity, and source orientation/pixel-aspect handling. Freeze operation
  order and hit-test inverse transforms before UI/export implementation.
- Unsupported fields and future schema versions produce a recoverable error;
  opening a project never silently discards edits.

### Engine and scene contracts

- Commands: load snapshot/revision, play, pause, seek with generation, set range,
  set monitor mute, resize, request diagnostics, shutdown.
- Events: ready, current time, presented frame timestamp/generation, buffering,
  structured error, capability change, and resource/performance statistics.
- `FrameLease`: asset/decode instance, source PTS/duration/time base, dimensions,
  pixel layout, color metadata, memory kind, lifetime token, synchronization.
  Do not expose unowned raw GPU pointers to UI code.
- Separate decode instances for two uses of one asset at different source times.
  Reuse compatible frames when requests actually match.
- A common scene evaluator emits ordered draw items and audio intervals. Preview
  and export consume the same transforms, visibility, timing, and asset revisions.
- SDR v1: documented working color space, linear-light blending policy,
  premultiplied-alpha convention, YUV range/matrix/chroma handling, and output
  conversion. Preview resolution changes sampling quality, not layout semantics.
- Shared text shaping/rasterization with resolved font identity, fallback order,
  glyph positions, and wrapping. Cache by font/content/style. Export must not
  independently reinterpret text through FFmpeg drawtext.

### Editing and storage contracts

- Validated commands mutate the model; each committed command has an undo inverse
  or bounded before/after record. One drag or text-edit session is one undo step.
- Save atomically; use explicit schema migrations and portable relative asset
  paths where possible. Keep absolute-location hints for relinking.
- Autosave to a distinct recovery file. Dirty-project Open/Home/Close behavior
  offers save/discard/cancel and does not silently replace a composition.
- Exports pin an immutable project snapshot. Later edits do not change the running
  export; source changes during export are detected and reported.

## 5. Subagent execution rules

This section is for a future implementation run. Do not launch work merely because
this plan exists. Once implementation is requested:

- Keep one coordinator and at most three implementation subagents active at once.
  Role names below are sequential assignments, not a request for unlimited agents.
- Start each assignment with the task ID, dependencies, allowed files, contracts,
  acceptance commands, and expected handoff artifacts. Subagents read relevant
  repository instructions and the current decision records first.
- The coordinator alone edits shared manifests, module exports, root app wiring,
  and cross-cutting contracts unless an exclusive handoff is made.
- Never have two agents edit the same module concurrently. Prefer new subsystem
  directories; integrate small working slices instead of merging a large rewrite.
- Do not replace user changes, reset the checkout, install system dependencies,
  or publish packages as an incidental part of a task. Follow actual authorization.
- A subagent reports changed files, behavior, exact checks/results, limitations,
  resource ownership concerns, and the next dependency. Distinguish tests actually
  run from planned tests and skipped hardware checks.
- Coordinator reviews lifetimes, synchronization, error propagation, and feature
  compatibility before enabling each slice. No task is complete solely because it
  compiles or mocks pass.

## 6. Work packages and dependency graph

Proposed paths below are new unless listed in section 2. Adjust names during the
contract phase, then keep ownership stable.

### P0 — Baseline, fixtures, and acceptance harness

Owner: validation subagent. Dependencies: none.
Files: `examples/compositor_bench.rs`, `tests/fixtures/` manifests/generators,
`docs/performance-baseline.md`, dedicated benchmark scripts.

1. Record revision/snapshot identity, CPU/GPU/driver, display backend/scaling,
   runtime versions, decoder selection, and current app behavior.
2. Generate redistributable fixtures: numbered moving frames; short/long GOP;
   1080p30/60 and 4K; VFR; 44.1/48 kHz audio with timed impulses; video without
   audio; rotation and non-square pixels; alpha image; Unicode paths and text.
3. Measure first-frame time, pause/play, seek-release-to-correct-frame latency,
   presented/dropped frames, CPU/RSS/GPU memory, resize, and repeated open/close.
4. Build JSON result output, fixed warmup/run lengths, repeated trials, and
   screenshot/frame/sample capture. Isolate fixture generation from timed runs.

Done: reproducible legacy baseline on the real GPU, plus a headless correctness
mode. Missing hardware is reported as unvalidated, never passed.

### P1 — Contracts and project core

Owner: model subagent under coordinator review. Dependencies: none; sync with P0.
Files: `src/project/{mod,time,assets,clips,commands,storage}.rs`, project tests.

Implement section 4, source-time mapping, edit validation, serialization,
migrations, undo/redo, and scene evaluation independent of GPUI/GPU. Add a
single-video adapter that reproduces today's trim/crop semantics.

Done: round-trip and migration tests; boundary/VFR mapping tests; overlapping
layers; command undo/redo; invalid values; missing assets; no float drift across
long projects. Coordinator approves the contracts for dependent agents.

### P2 — FFmpeg library runtime and software decoder

Owner: media-runtime subagent. Dependencies: initial contracts from P1.
Files: `src/engine/decode/`, `src/engine/ffi/`, new runtime build script and lock.
Coordinator integrates Cargo changes.

Build explicit FFmpeg library/binding linkage and ABI validation. Preserve the
existing CLI runtime. Implement demux, video/audio decode, packet draining,
seek/flush, source metadata/orientation, bounded queues, generation cancellation,
error handling, and independent decode instances. Keep original files read-only.

Done: software-decoded frames/audio match fixtures, seek after EOF works,
corrupted input fails cleanly, shutdown is bounded, and packaged library lookup
does not depend on development PATH or accidental system libraries.

### P3 — Vulkan rendering and presentation spike

Owner: GPU subagent. Dependencies: P0 measurements; P1 frame/scene contracts.
Files: `src/engine/gpu/`, a dedicated spike example, presentation decision record.
Use synthetic images first; P2 decoding can follow without blocking the spike.

Create device/queues, reusable YUV/RGBA resources, shader pipeline, offscreen
composition, and presentation. Investigate both section 3.1 routes. Validate
selection drawing/input coordinates, resize, modal handling, and device loss.
Compile/cache pipelines outside live pointer interactions.

Done: select and document one route with measured overhead. Run Vulkan validation
layers in development; no unresolved validation errors. One-video P2 integration
must meet the baseline gate before broader engine/UI migration.

### P4 — Hardware decode and frame interoperability

Owner: GPU/media interoperability subagent. Dependencies: P2 + P3 decision.
Files: `src/engine/decode/hardware/`, `src/engine/gpu/import/`, interoperability tests.

Implement capability probing and the reference hardware path. Explicitly handle
device identity, external-memory lifetimes, pixel formats, synchronization, and
pool exhaustion. Use software decode/upload fallback when necessary. Additional
vendors are independently certified; do not delay all correctness work on them.

Done: actual hardware decoder is recorded; transfer path is measured; repeated
seek/resize/close has no stale textures, deadlocks, or unbounded memory. Compare
with P2 fallback, including whether hardware helps the reference workload.

### P5 — Playback scheduler, audio mixer, and backend interface

Owner: playback subagent. Dependencies: P1 + P2 + P3; P4 plugs in when ready.
Files: `src/engine/{api,scheduler,clock,audio,cache,legacy}.rs` with coordinator
approval for `api.rs` changes.

Implement one project clock, visible-clip scheduling, bounded prefetch, generation
seeks, pause/range/EOF behavior, audio resampling/mixing and latency accounting.
Define gain summation and a shared clipping policy for preview/export. Monitor
mute does not change saved gains or exported audio. Treat hidden video and muted
audio as separate scheduling decisions. Preserve current legacy backend behind
the same high-level transport interface where practical.

Done: two videos remain aligned; timed audio impulses verify sync; silent and
audio-only projects work; rapid seeks settle correctly; no UI-thread waits;
resource and callback constraints are tested; audio-device loss is recoverable.

### P6 — Shared visual composition and text

Owner: composition subagent. Dependencies: P1 + P3; coordinate P5 scheduling.
Files: `src/composition/{scene,transforms,color,text,images,shapes}.rs`, shaders
assigned exclusively from `src/engine/gpu/` during this task.

Implement canonical transforms, crop, alpha, layer order, color conversion,
orientation, images and rectangles, shaped text, font fallback, and glyph caching.
Font substitutions on reopen must be visible; persist enough information to
resolve the same font when available. Use the same layout for hit testing and
offline export. Handle half-resolution preview without changing canvas positions.

Done: reference frames cover rotated/cropped layers, alpha edges, gradients,
mixed frame sizes, Unicode/multiline text, and missing fonts. SDR policy is
documented and HDR behavior is explicit.

### P7 — Editor session, asset import, and persistence UI

Owner: editor-session subagent. Dependencies: P1; engine wiring after P5.
Files: `src/ui/project_session.rs`, `src/ui/asset_panel.rs`, storage dialogs.
Coordinator integrates `src/ui.rs`, `actions.rs`, `file_drop.rs`, and worker wiring.

Replace singleton edit assumptions with a project session. Open creates a
single-video project; Add Media imports all supported selected/dropped files.
Keep assets distinct from their timeline instances. Key inspection/thumbnail/
waveform jobs by asset and generation. Add save/load/relink, dirty-state prompts,
recovery, and undo/redo shortcuts. Retain Home/settings behavior.

Done: multi-file import, reopen, missing/moved media, autosave recovery, and undo
work without losing an existing project. Late workers cannot update the wrong asset.

### P8 — Timeline and transport editing

Owner: timeline subagent. Dependencies: P1 + P7 contracts; P5 for live transport.
Files: `src/ui/timeline.rs`, new timeline helper modules, relevant interaction tests.

Add track rows, zoom/scroll, playhead, selection, clip movement/trim/split, source
offsets, layer order, snapping, lock/visibility, audio controls, and waveforms.
Keep time inputs as views of the model. Distinguish project duration, selected
clip bounds, and work/export range. Group drag edits into one undo transaction.
Virtualize offscreen timeline content and bound waveform/thumbnail requests.

Done: end-exclusive boundaries are correct, moving a clip does not change its
source trim, trim cannot exceed source bounds, locked clips do not mutate,
playhead stays responsive during zoom/drag, and EOF/replay behavior is consistent.

### P9 — Canvas tools and inspector

Owner: canvas subagent. Dependencies: P6 + P7; selected presentation route P3.
Files: `src/ui/canvas_tools.rs`, `src/ui/layer_inspector.rs`, assigned portions of
`preview_panel.rs` and `crop.rs` after exclusive coordinator handoff.

Implement selection/hit testing, drag/resize/rotate/crop, guides, aspect locking,
z-order commands, text editing, and property controls. Convert pointer coordinates
through display scale, preview letterboxing, canvas scale, and inverse transforms.
Draw handles through the chosen preview route, not behind a native child window.
Transform edits on a paused frame must reuse it without seeking or decoding.

Done: preview and inspector agree, no jumps at 100/150/200% display scaling,
rotated hit tests work, text typing/undo are coherent, and modals remain visible.

### P10 — Deterministic composition export

Owner: export subagent. Dependencies: P1 + P2 + P5 audio + P6.
Files: `src/export/`, narrowly scoped changes to `src/job.rs`; coordinator handles
`src/ui/export_controls.rs` integration.

Use the shared scene evaluator and offscreen compositor at output resolution.
Iterate exact rational frame times and audio sample intervals; no preview frame
dropping, proxies, UI handles, or monitor mute in exports. Use original assets.
Feed composed frames/audio to pinned FFmpeg encoding/muxing. Start with a bounded
CPU readback/encoder path if necessary; hardware encoding/interoperability is an
optional measured optimization and must not block a correct full export.

Preserve MP4/MKV MPEG-4/AAC behavior initially; do not assume H.264 encoding is
available. Provide composition WAV via shared audio mixing and GIF via the
existing palette capability or equivalent tested encoder path. Keep unsupported
formats disabled accurately. Preserve legacy fast-cut CLI for eligible simple
requests; composition export always renders/encodes.

Reuse/refactor job cancellation, progress, temporary-file ownership, bounded logs,
atomic no-overwrite publication, and failure cleanup. Validate output against all
project source paths. Exporting a snapshot while editing is safe; throttle GPU
export submissions so interactive preview remains responsive. Report errors
without deleting user media or a previous successful output.

Done: decoded exports match scene references with codec-appropriate tolerances;
duration/end frames/audio are correct; cancellation, ENOSPC, encoder failure,
destination races, Unicode paths, and app close leave no published partial file.

### P11 — Proxies, caches, and performance hardening

Owner: performance subagent. Dependencies: P0 + P4/P5 + P8/P9 + P10.
Files: `src/engine/proxy/`, assigned cache modules, benchmark scenarios.

Add optional background proxies and expensive-scene preview caches using explicit
disk/RAM/GPU budgets and eviction. Keys include asset identity, source revision,
decode time, transform/effect revision where applicable, and proxy specification.
Preserve source-time mapping, especially for VFR; proxies cannot shift edits.
Use low-priority cancellable workers. Full-resolution originals remain the export
source. Add visible proxy/readiness status and cleanup controls.

Profile before optimizing: pipeline compilation, frame copies, decoder threads,
queue occupancy, cache churn, draw submissions, GPU stalls. Avoid decoding inactive
clips; prewarm upcoming clips briefly. Lower preview render resolution explicitly;
do not imply it lowers full-resolution decode cost without proxies.

Done: P0 reference workloads meet section 8 gates; transform-only drags do not
trigger decoding; caches stabilize and recover after media changes; proxy/export
timing matches originals; overload behavior is bounded and understandable.

### P12 — Packaging, integration acceptance, and rollout

Owner: release/validation subagent; coordinator owns final integration.
Dependencies: runtime packaging begins after P2; release requires P7–P11.
Files: `scripts/package-linux*.sh`, `scripts/smoke-linux-bundle.sh`, packaging
manifests/notices, `docs/packaging.md`, `docs/validation.md`, README.

Bundle exact FFmpeg libraries, bindings/runtime version manifest, shaders, required
fonts/assets, and retained libmpv fallback. Verify codec and hardware capabilities
from the installed artifact. Do not bundle vendor drivers or silently discover
unrelated system FFmpeg. Audit existing notices/source packaging for changed
dependencies. Keep `--no-default-features` core/CLI workflow working; Vulkan-heavy
features must be feature-gated if necessary.

Run installed-app acceptance without development overrides. Exercise Home/open,
multi-import, editing, save/reopen/relink, recovery, preview, mixed-audio export,
cancel/failure, resize, settings, clipboard, and shutdown. Document tested hardware
and unsupported platforms accurately. Add engine selection diagnostics/feature
flag; enable the new engine by default only after gates pass.

Legacy fallback may handle compatible single-video projects only. Never flatten
or drop layers silently if the new engine is unavailable. Preserve project data
and offer an explicit unsupported-engine state for complex projects.

Done: packaged end-to-end checklist passes, performance evidence is attached,
known limits are documented, and rollback retains users' projects and originals.

## 7. Dispatch schedule for a coordinator plus three subagents

| Wave | Slot A | Slot B | Slot C | Coordinator responsibility |
| --- | --- | --- | --- | --- |
| 0 | P0 baseline | P1 contracts/model | P2 runtime investigation, implementation after contract freeze | Protect checkout; freeze interfaces |
| 1 | P3 Vulkan/presentation | P2 decode completion | P7 storage/session against model | Select presentation; run one-video gate |
| 2 | P4 hardware sharing | P5 scheduler/audio | P6 visuals/text | Integrate a two-video/text vertical slice |
| 3 | P8 timeline | P9 canvas/inspector | P10 export | Own shared UI wiring; verify preview/export parity |
| 4 | P11 performance/proxies | P12 packaging | Cross-subsystem acceptance and bug fixes with assigned files | Run release gates; review remaining issues |

Tasks can overlap only when their interface dependencies are satisfied. A runtime
or shader file owned by one task must be handed off before another edits it.
Do not parallelize unresolved architecture decisions into competing implementations.

Critical path: P0/P1 → P2/P3 → P5/P6 → P8/P9/P10 → P11/P12. P4 must pass before
claiming accelerated decoding, but software fallback allows correctness work to
continue. If P3 fails the performance gate, UI work can continue against contracts
while GPU work is corrected; do not call the engine complete.

## 8. Verification and performance gates

Existing regression commands (use the repository's documented explicit runtimes):

```sh
cargo fmt --check
cargo test --locked --no-default-features
cargo check --locked
SLICER_TEST_FFMPEG_DIR="$PWD/build/ffmpeg/linux-x86_64/bin" \
SLICER_FFMPEG_DIR="$PWD/build/ffmpeg/linux-x86_64/bin" \
  cargo test --locked --no-default-features -- --ignored
SLICER_MPV_LIBRARY=/absolute/path/to/libmpv.so.2 \
  cargo test --locked --test native_player_integration
```

Coordinator adds explicit feature-enabled composition checks and benchmark
commands when those targets exist; do not document fictional commands as passing.
Required release fixtures/runtime/GPU absence must fail the release harness,
even if ordinary developer tests support a skip.

### Correctness matrix

- Project: schema versions, atomic saves/recovery, relative paths, missing assets,
  duplicate asset instances, undo/redo, invalid and non-finite inputs.
- Video: source FPS differing from project FPS, VFR, B-frame reordering, rotation,
  pixel aspect, range/matrix metadata, end-exclusive timing, EOF then seek/replay.
- Composition: overlap/order, off-canvas elements, crop/rotation/opacity,
  premultiplied alpha, text shaping/wrapping/fonts, SDR output behavior.
- Audio: different sample rates, gaps/overlaps, resampling, gain/mute, monitor mute,
  source-without-audio, seek flushing, audio-only project, output-device loss.
- Interaction: rapid scrubbing, paused dragging, high-DPI resize, modal visibility,
  dirty-project navigation, keyboard focus, multi-file drop, repeated open/close.
- Export: shared scene output, originals despite proxies, exact frame/sample
  schedule, codec tolerance, error/cancel/disk-full/no-overwrite behavior.
- GPU: validation layers, ownership under cancellation, unsupported import,
  exhausted pools, device loss, orderly surface destruction, bounded resource use.

### Proposed budgets, ratified after P0

These are goals to test, not results already achieved. Freeze fixture hashes,
preview/output dimensions, decoder path, cache state, and hardware for comparisons.

1. Single 1080p60 video: no sustained missed deadlines; at most 0.1% missed/dropped
   scheduled frames over a 60-second steady-state run after warmup. P95 seek and
   first-frame latency should be within 10% of baseline or 20 ms, whichever
   allowance is larger. Investigate trial variance before declaring regression.
2. Two simultaneous 1080p60 videos plus text at a 1080p preview: target sustained
   60 fps on the reference RTX machine, with no audio underruns and <= 0.1% missed
   frame deadlines in the steady-state test. Log each stream's actual decoder.
3. Four 1080p streams and two 4K streams: characterization workloads, not a promise
   of full-quality 60 fps. Record maximum sustainable settings and proxy benefits.
4. Input-to-visible transform response: P95 <= 50 ms under the two-video workload;
   a paused transform should not cause new source decoding.
5. Seek release to correct presented frame: target P95 <= 150 ms for the agreed
   local 1080p fixture. Report cold and warm long-GOP seeks separately; never hide
   the worst workload behind an average.
6. A/V offset <= 20 ms in measured impulse/frame fixtures during steady playback;
   no cumulative drift over a 10-minute run. Document instrumentation accuracy.
7. GPU composition/presentation work must fit within the 16.7 ms 60-fps interval
   with headroom; separately report decode, GPU, queue wait, and end-to-end timing.
   GPU timestamp support is optional, and CPU submit timing is not GPU duration.
8. Memory: establish and freeze absolute RAM/VRAM budgets after P0/P3. All queues
   and caches have configured bounds. Repeated open/seek/close cycles return to a
   stable retained-resource plateau; no monotonic leak after warmup.
9. A 30-minute edit/play/resize/export/cancel soak has no deadlock, validation
   error, persistent black/stale frame, runaway queue, or surviving worker.

Capture preview offscreen frames before presentation for comparison with export.
Compare lossless intermediates strictly within documented color/raster tolerances;
decode lossy outputs and use codec-appropriate thresholds. Validate geometry,
timing, glyph placement, and audio separately so one global image metric cannot
hide a shifted layer or wrong frame.

## 9. Risks and required responses

| Risk | Response |
| --- | --- |
| Second Vulkan device costs too much memory | P3 measures it; investigate proven GPUI integration before expanding the engine |
| Hardware-frame import unavailable on a driver/format | Explicit software/upload or measured copy fallback; capability reporting |
| Native child covers UI overlays | Draw canvas tools in compositor; preserve surface hide/remap for modals |
| Color or text differs on export | Shared scene/layout/color code; common fonts; parity fixtures |
| Multi-video audio drifts | One clock, latency-aware sample accounting, flush generations, long-run checks |
| Resource pools deadlock during seeking | Bounded queues, cancellation, frame leases, retirement tests and traces |
| Export stalls live editing | Separate scheduling priorities and bounded submissions; snapshot export |
| Proxy shifts VFR edits | Preserve source-time mapping and test frame identity; fall back to originals |
| Packaging accidentally relies on dev machine | Installed-artifact smoke tests without overrides; explicit ABI/runtime manifest |
| Scope grows beyond a shippable editor | Finish section 1 and release gates before animation/effects/platform expansion |

## 10. Handoff template for every future subagent

```text
Task: P<number> — <name>
Objective: <observable result>
Dependencies: <completed task IDs and contract versions>
Allowed files: <exclusive paths>
Read first: this plan, applicable repository instructions, relevant decision records
Implement: <bounded checklist from the task>
Do not change: shared contracts/manifests/root wiring without coordinator handoff
Acceptance: <specific tests, fixtures, and required hardware evidence>
Return: changed files, implementation summary, exact checks/results, known limits,
        resource/lifetime considerations, and ready-to-integrate status
```

The coordinator's final completion report must include a usable packaged build,
project round-trip evidence, a two-video/text/audio export example, baseline versus
new-engine measurements, certified runtime/hardware paths, and unresolved limits.
Do not declare completion with placeholder UI, synthetic-only playback, a missing
export path, or skipped required hardware tests.

## 11. Effort and reassessment

Previous conversation estimates were rough, not measured schedules. Treat 1–2
weeks for a one-video spike, a further 2–4 for a layered playback slice, and a
further 4–8 for an integrated first editor as provisional planning ranges only.
This full plan also includes persistence, proxies, packaging, failure handling,
and performance certification; hardware interoperability can push it beyond that
range. Subagents reduce independent implementation time, not integration risk.

Re-estimate after P0–P3 using working code and measurements, then after P5/P6/P10
using a complete preview-to-export slice. Maintain remaining task estimates and
blocking evidence. Do not promise a delivery date or Adobe-equivalent performance
before those gates have been demonstrated.
