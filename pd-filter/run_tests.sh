#!/bin/sh
# Regenerate the golden cases from the Python code, then run the Rust tests against them.
# Usage (from anywhere):  native/pd-filter/run_tests.sh      (set CARGO_TARGET_DIR to keep build output out of the repo)
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
.venv/bin/python native/pd-filter/tests/gen_golden.py
cd native && cargo test -p pd-filter "$@"
