#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Run the current checkout, never install or replace the user's launcher.
exec cargo run --manifest-path "$ROOT_DIR/Cargo.toml" --offline --locked --release --bin danmu -- "$@"
