#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-}"
ASSET_NAME="shisui-danmu-macos-universal.tar.gz"
DIST_DIR="$ROOT_DIR/dist"

if [[ ! "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
  echo "用法: ./script/package_release.sh v<major>.<minor>.<patch>" >&2
  exit 2
fi

STAGE_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGE_DIR"' EXIT

cd "$ROOT_DIR"
swift build \
  -c release \
  --arch arm64 \
  --arch x86_64 \
  --product ShisuiDanmuTerminal

BIN_DIR="$(swift build -c release --arch arm64 --arch x86_64 --show-bin-path)"
SOURCE="$BIN_DIR/ShisuiDanmuTerminal"

lipo "$SOURCE" -verify_arch arm64 x86_64
install -m 0755 "$SOURCE" "$STAGE_DIR/danmu"
install -m 0644 LICENSE THIRD_PARTY_NOTICES.md "$STAGE_DIR/"
printf '%s\n' "$VERSION" > "$STAGE_DIR/VERSION"

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"
COPYFILE_DISABLE=1 tar -C "$STAGE_DIR" -czf "$DIST_DIR/$ASSET_NAME" \
  danmu LICENSE THIRD_PARTY_NOTICES.md VERSION
(
  cd "$DIST_DIR"
  shasum -a 256 "$ASSET_NAME" > "$ASSET_NAME.sha256"
)

echo "已生成：$DIST_DIR/$ASSET_NAME"
echo "已生成：$DIST_DIR/$ASSET_NAME.sha256"
