#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

VERSION="${1:-local}"
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
  Darwin) PLATFORM="macos" ;;
  Linux) PLATFORM="linux" ;;
  *) echo "不支持的打包平台：$OS" >&2; exit 1 ;;
esac
case "$ARCH" in
  arm64|aarch64) ARCHIVE_ARCH="aarch64" ;;
  x86_64|amd64) ARCHIVE_ARCH="x86_64" ;;
  *) echo "不支持的处理器架构：$ARCH" >&2; exit 1 ;;
esac

./script/verify.sh
mkdir -p dist
ASSET="shisui-danmu-${PLATFORM}-${ARCHIVE_ARCH}.tar.gz"
tar -czf "dist/$ASSET" -C target/release danmu
if command -v shasum >/dev/null 2>&1; then
  (cd dist && shasum -a 256 "$ASSET" > "$ASSET.sha256")
else
  (cd dist && sha256sum "$ASSET" > "$ASSET.sha256")
fi
printf '%s\n' "$VERSION" > dist/VERSION
echo "已生成：dist/$ASSET"
