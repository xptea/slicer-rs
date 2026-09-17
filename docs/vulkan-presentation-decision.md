# Vulkan presentation spike decision record

Status: contract-only spike; no production presentation route selected or
validated yet.

## Context

The layered-editor plan keeps direct Vulkan as the engineering direction, while
the current Linux preview uses libmpv's native child surface and OpenGL/EGL by
default. The current `Cargo.toml` has no direct Vulkan API dependency. The
Vulkan-related crates visible in `Cargo.lock` are transitive dependencies of
the pinned GPUI stack and are not a usable backend contract for this crate
without changing the shared manifest.

This change therefore stays within the allowed GPU/example/test/docs paths and
does not touch `src/lib.rs`, Cargo manifests, the existing native player, or
vendor code.

## Decision at this stage

No production route is declared. The owned Vulkan child-surface route remains
the leading validation target because it matches the existing native-surface
boundary and avoids assuming image ownership or synchronization with GPUI. The
offscreen Vulkan-to-GPUI import route remains a separate candidate with more
requirements. This is a sequencing decision, not evidence that either route
works on the reference machine.

The next real spike must select one route only after a direct Vulkan backend can
create and destroy the required objects on the target system, exercise resize
and device loss, and record the measurements below. Until then, the existing
libmpv/OpenGL presentation path is unchanged.

## Implemented contract

`src/engine/gpu/mod.rs` is an FFI-free, contract-only module. It creates no
Vulkan loader, instance, physical device, logical device, queue, swapchain,
surface, image, semaphore, fence, or OS handle. Its opaque IDs are test/future-
adapter tokens only.

The contract contains:

- `PresentationRoute` for the owned child-surface and offscreen GPUI-import
  routes.
- `Capability`, `CapabilityStatus`, `CapabilityReport`, and `RouteReadiness`.
  `NotProbed`, `Observed`, `Validated`, and `Unavailable` are intentionally
  distinct; activation requires every route requirement to be `Validated`.
- `PresentationSession` states `Created`, `Ready`, `Suspended`, `Resizing`,
  `DeviceLost`, ordered `ShuttingDown`, and `Shutdown`.
- Opaque target tokens, non-zero extents, resize/minimize transitions, and a
  monotonically increasing generation for invalidating stale work.
- A resource ledger for imported frames, reusable YUV/RGBA storage,
  composition/presentable images, and pipelines. Submitted resources become
  `InFlight`, must be explicitly retired, and can only then be released or
  reused.
- Device loss invalidates owned resources as `Lost`, clears submissions, resets
  route capability evidence to `NotProbed`, and requires resource release plus
  fresh validation before recovery.
- Shutdown ordering: cancel producers, stop audio, retire GPU work, release
  imported frames, destroy render resources, and destroy the presentation
  target last.

The example and tests include the module explicitly. This is deliberate: there
is no production wiring or behavior change in this spike.

## Route capability matrix

| Route | Required evidence in this contract | Current evidence |
| --- | --- | --- |
| Owned Vulkan child surface | Vulkan loader/device, graphics and present queues, reusable image resources, composition pipeline, native child surface | Not probed |
| Offscreen Vulkan imported into GPUI | Vulkan loader/device, graphics queue, reusable image resources, composition pipeline, offscreen composition, GPUI image import, external memory, external synchronization, adapter match, layout/ownership transitions | Not probed |

An `Observed` capability is still insufficient for activation. A future probe
must record the device identity, queue family, format/modifier/allocation,
synchronization primitive, image layout transitions, and teardown result before
marking a capability `Validated`.

## Measurements and validation status

No Vulkan measurements were taken in this design/compile spike:

| Gate | Result | Why it remains open |
| --- | --- | --- |
| Vulkan loader/device creation | Not run | No direct Vulkan backend/dependency was added |
| Native child-surface presentation | Unvalidated | No real surface or swapchain was created |
| Offscreen image import into GPUI | Unvalidated | Adapter sharing, external memory, synchronization, and layouts were not tested |
| Validation-layer output | Not run | No Vulkan instance was created |
| Resize/minimize behavior on hardware | Unvalidated | Tests exercise contract transitions with synthetic tokens only |
| Device loss/recovery on hardware | Unvalidated | Tests exercise invalidation/release ordering only |
| CPU/GPU latency, frame deadlines, memory/RAM/VRAM | Not measured | No renderer or workload exists in this spike |
| Hardware decode/frame interoperability | Out of scope | P4 depends on a selected P3 route |

The lifecycle tests are correctness checks for the contract, not proof of
Vulkan behavior, performance, driver support, or hardware-frame import.

## Required follow-up for a real presentation spike

1. Add a direct Vulkan dependency and backend in a separately approved change;
   do not infer it from the transitive lockfile.
2. Run the owned-child route first with synthetic RGBA/YUV inputs, validation
   layers, explicit queue/device identity, and an instrumented teardown.
3. Run the offscreen route only if the pinned GPUI renderer exposes a proven
   import boundary. Verify adapter matching, memory allocation/modifiers,
   image ownership/layout transitions, semaphore/fence handoff, and teardown.
4. Exercise repeated resize, minimize/restore, cancellation, device loss, and
   shutdown. Capture validation output and retained resource counts.
5. Measure the plan's one-video gate: P95 seek/first-frame latency, missed
   presentation deadlines, CPU/GPU/queue/end-to-end time, and stable RAM/VRAM
   plateau. Compare both routes against the existing libmpv/OpenGL baseline.
6. Update this record with exact hardware, driver, fixture, runtime, measured
   values, validation result, and one selected production route before any root
   module wiring or migration.

## Verification for this change

The isolated checks completed without requiring a Vulkan device:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Pass |
| `cargo check --locked --no-default-features --example vulkan_spike` | Pass |
| `cargo test --locked --no-default-features --test gpu_lifecycle` | Pass: 7 passed, 0 failed |
| `git diff --check` | Pass |

The focused test command emitted one unrelated existing warning for an unused
`waveform` import in `src/main.rs`; it did not affect the result. No production
module wiring or shared manifest was changed by this spike.
