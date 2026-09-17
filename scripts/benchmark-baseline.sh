#!/bin/sh
# Run the headless oracle and record an explicit baseline report.
set -eu

fixture=${1:-tests/fixtures/compositor_scene.json}
report=${2:-build/validation/p0-baseline.json}
mkdir -p "$(dirname "$report")"

if [ ! -f "$fixture" ]; then
    echo "benchmark-baseline: fixture is missing: $fixture" >&2
    exit 2
fi

set +e
cargo run --locked --no-default-features --example compositor_bench -- --fixture "$fixture" > "$report"
status=$?
set -e
if [ "$status" -ne 0 ]; then
    echo "benchmark-baseline: headless oracle failed; report retained at $report" >&2
    exit "$status"
fi
echo "benchmark-baseline: report written to $report"

