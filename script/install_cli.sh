#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_DIR="${DANMU_INSTALL_DIR:-$HOME/.local/bin}"
BINARY_NAME="danmu"
if [[ "${OS:-}" == "Windows_NT" ]]; then
  BINARY_NAME="danmu.exe"
fi

cd "$ROOT_DIR"
cargo build --locked --release
mkdir -p "$INSTALL_DIR"
install -m 0755 "target/release/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"

"$INSTALL_DIR/$BINARY_NAME" --version
echo "已安装：$INSTALL_DIR/$BINARY_NAME"
