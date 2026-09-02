#!/usr/bin/env bash

set -euo pipefail

REPOSITORY="rockythink/shisui-danmu"
INSTALL_DIR="${DANMU_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${DANMU_VERSION:-latest}"
UNINSTALL=false

usage() {
  cat <<'EOF'
用法: install_release.sh [--version vX.Y.Z] [--install-dir PATH] [--uninstall]
环境变量: DANMU_VERSION、DANMU_INSTALL_DIR、DANMU_RELEASE_BASE_URL
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="${2:?缺少版本号}"; shift 2 ;;
    --install-dir) INSTALL_DIR="${2:?缺少安装目录}"; shift 2 ;;
    --uninstall) UNINSTALL=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "未知参数：$1" >&2; usage >&2; exit 2 ;;
  esac
done

OS="$(uname -s)"
ARCH="$(uname -m)"
BINARY_NAME="danmu"
FORMAT="tar.gz"
case "$OS" in
  Darwin) PLATFORM="macos" ;;
  Linux) PLATFORM="linux" ;;
  MINGW*|MSYS*|CYGWIN*) PLATFORM="windows"; FORMAT="zip"; BINARY_NAME="danmu.exe" ;;
  *) echo "不支持的操作系统：$OS" >&2; exit 1 ;;
esac
case "$ARCH" in
  arm64|aarch64) ARCHIVE_ARCH="aarch64" ;;
  x86_64|amd64) ARCHIVE_ARCH="x86_64" ;;
  *) echo "不支持的处理器架构：$ARCH" >&2; exit 1 ;;
esac
if [[ "$PLATFORM" == "windows" && "$ARCHIVE_ARCH" != "x86_64" ]]; then
  echo "Windows Release 当前仅提供 x86_64" >&2; exit 1
fi

DESTINATION="$INSTALL_DIR/$BINARY_NAME"
if [[ "$UNINSTALL" == true ]]; then
  rm -f "$DESTINATION"
  echo "已卸载：$DESTINATION"
  exit 0
fi
if [[ "$VERSION" != "latest" && ! "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
  echo "版本格式必须为 vX.Y.Z" >&2; exit 2
fi

ASSET_NAME="shisui-danmu-${PLATFORM}-${ARCHIVE_ARCH}.${FORMAT}"
if [[ -n "${DANMU_RELEASE_BASE_URL:-}" ]]; then
  BASE_URL="${DANMU_RELEASE_BASE_URL%/}"
elif [[ "$VERSION" == "latest" ]]; then
  BASE_URL="https://github.com/$REPOSITORY/releases/latest/download"
else
  BASE_URL="https://github.com/$REPOSITORY/releases/download/$VERSION"
fi

TEMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TEMP_DIR"' EXIT
ARCHIVE="$TEMP_DIR/$ASSET_NAME"
CHECKSUM="$ARCHIVE.sha256"

download() {
  if command -v curl >/dev/null 2>&1; then curl -fL --retry 3 --connect-timeout 15 "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then wget -q "$1" -O "$2"
  else echo "需要 curl 或 wget" >&2; exit 1; fi
}

echo "正在下载拾穗弹幕台 TUI ${VERSION}（${PLATFORM}/${ARCHIVE_ARCH}）…"
download "$BASE_URL/$ASSET_NAME" "$ARCHIVE"
download "$BASE_URL/$ASSET_NAME.sha256" "$CHECKSUM"
if command -v shasum >/dev/null 2>&1; then
  (cd "$TEMP_DIR" && shasum -a 256 -c "$(basename "$CHECKSUM")")
else
  (cd "$TEMP_DIR" && sha256sum -c "$(basename "$CHECKSUM")")
fi

if [[ "$FORMAT" == "zip" ]]; then unzip -q "$ARCHIVE" -d "$TEMP_DIR"; else tar -xzf "$ARCHIVE" -C "$TEMP_DIR"; fi
if [[ ! -f "$TEMP_DIR/$BINARY_NAME" ]]; then echo "Release 内缺少 $BINARY_NAME" >&2; exit 1; fi
mkdir -p "$INSTALL_DIR"
install -m 0755 "$TEMP_DIR/$BINARY_NAME" "$DESTINATION"
echo "已安装：$DESTINATION"
case ":$PATH:" in *":$INSTALL_DIR:"*) ;; *) echo "请将 $INSTALL_DIR 加入 PATH" ;; esac
