# P0 benchmark fixtures

`compositor_scene.json` is a small, checked-in synthetic scene. It is rendered
by `examples/compositor_bench.rs` without a GPU, FFmpeg, image crate, or other
runtime dependency. Its fixed canvas, layer order, alpha values, motion, and
Unicode text make the correctness checks reproducible on every host.

`manifest.json` describes the optional media set. The media files are generated
outside the source tree by `scripts/benchmark-fixtures.sh` because the 1080p and
4K files are generated artifacts rather than source fixtures. The generator
uses an explicitly supplied FFmpeg directory and also writes deterministic
numbered PPM frames plus 44.1/48 kHz PCM impulse WAV inputs.

The generated directory is normally:

```text
build/validation/p0-fixtures/
├── media/
├── numbered-frames/
├── headless.json
└── fixture-generation.json
```

Generated media is not a validation result by itself. The benchmark report
records per-fixture FFprobe checks as `passed`, `failed`, or `unvalidated`.
Missing FFmpeg, missing generated files, and missing display/GPU inputs are
`unvalidated`; they are never converted into a passing result.
