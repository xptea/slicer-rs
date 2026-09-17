# Performance and correctness baseline

The repository now includes a deterministic, headless P0 oracle and an
optional media-fixture generator. The oracle is a correctness check for the
layered renderer contract; it is not evidence of Vulkan performance.

## Headless oracle

Run from the repository root:

```sh
cargo run --locked --no-default-features --example compositor_bench -- --human
scripts/benchmark-baseline.sh
```

The JSON report contains the fixture identity, stable FNV-1a frame hashes,
alpha/layer pixel checks, elapsed CPU time, and explicit `gpu: unvalidated` and
`decoder: synthetic` fields. A failing check exits non-zero. The hash is a
stable regression signal, not a cryptographic media identity.

## Optional media fixtures

Media generation never searches `PATH` implicitly. Supply the exact bundled
FFmpeg directory:

```sh
scripts/benchmark-fixtures.sh "$PWD/build/ffmpeg/linux-x86_64/bin"
```

Generated media belongs under `build/validation/p0-fixtures/` and is ignored by
Git. The checked-in `tests/fixtures/manifest.json` describes the expected
streams and tags. A missing FFmpeg bundle or missing GPU/display is reported as
unvalidated by a release harness; it must not be converted into a passing
performance result.

## Required real measurements before claiming the gates

The following still require an instrumented production decoder/compositor and a
fixed reference machine/media set: first-frame and seek P95, dropped/missed
frames, A/V impulse offset, CPU/RSS/GPU memory, GPU/queue timing, resize and
open/close soak. Capture the environment, driver, display scaling, decoder
path, warmup, trial count, fixture hashes, and whether the route used CPU
readback/upload. No Vulkan measurement is claimed by the current headless
oracle.

