#!/usr/bin/env bash

set -euo pipefail

REPOSITORY="rockythink/shisui-danmu"
INSTALL_DIR="${DANMU_INSTALL_DIR:-$HOME/.local/bin}"
DESTINATION="$INSTALL_DIR/danmu"
VERSION="${DANMU_VERSION:-latest}"
ASSET_NAME="shisui-danmu-macos-universal.tar.gz"

usage() {
  cat <<'EOF'
用法: install_release.sh [--uninstall]

默认下载最新 GitHub Release，并安装到 ~/.local/bin/danmu。
可用环境变量：

  DANMU_INSTALL_DIR="$HOME/bin"  指定安装目录
  DANMU_VERSION=v0.1.0           安装指定版本
EOF
}

case "${1:-}" in
  --uninstall)
    if [[ -e "$DESTINATION" ]]; then
      rm "$DESTINATION"
      echo "已卸载 $DESTINATION"
    else
      echo "$DESTINATION 不存在，无需卸载"
    fi
    exit 0
    ;;
  -h|--help)
    usage
    exit 0
    ;;
  "") ;;
  *)
    usage >&2
    exit 2
    ;;
esac

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "拾穗弹幕台 TUI 当前只支持 macOS。" >&2
  exit 1
fi

case "$(uname -m)" in
  arm64|x86_64) ;;
  *)
    echo "不支持的 Mac 架构：$(uname -m)" >&2
    exit 1
    ;;
esac

if [[ "$VERSION" != "latest" && ! "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
  echo "DANMU_VERSION 必须是 latest 或 v<major>.<minor>.<patch>。" >&2
  exit 2
fi

if [[ -n "${DANMU_RELEASE_BASE_URL:-}" ]]; then
  BASE_URL="${DANMU_RELEASE_BASE_URL%/}"
  CURL_SECURITY_ARGS=(--proto '=file,https' --tlsv1.2)
elif [[ "$VERSION" == "latest" ]]; then
  BASE_URL="https://github.com/$REPOSITORY/releases/latest/download"
  CURL_SECURITY_ARGS=(--proto '=https' --tlsv1.2)
else
  BASE_URL="https://github.com/$REPOSITORY/releases/download/$VERSION"
  CURL_SECURITY_ARGS=(--proto '=https' --tlsv1.2)
fi

TEMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TEMP_DIR"' EXIT

ARCHIVE="$TEMP_DIR/$ASSET_NAME"
CHECKSUM="$TEMP_DIR/$ASSET_NAME.sha256"

download() {
  local url="$1"
  local output="$2"
  curl "${CURL_SECURITY_ARGS[@]}" --fail --silent --show-error --location \
    --retry 3 --output "$output" "$url"
}

echo "正在下载拾穗弹幕台 TUI ${VERSION}…"
download "$BASE_URL/$ASSET_NAME" "$ARCHIVE"
download "$BASE_URL/$ASSET_NAME.sha256" "$CHECKSUM"

(
  cd "$TEMP_DIR"
  shasum -a 256 -c "$(basename "$CHECKSUM")"
)

tar -xzf "$ARCHIVE" -C "$TEMP_DIR"
if [[ ! -x "$TEMP_DIR/danmu" ]]; then
  echo "Release 归档缺少可执行文件 danmu。" >&2
  exit 1
fi

mkdir -p "$INSTALL_DIR"
TEMP_DESTINATION="$DESTINATION.tmp.$$"
trap 'rm -rf "$TEMP_DIR"; rm -f "$TEMP_DESTINATION"' EXIT
install -m 0755 "$TEMP_DIR/danmu" "$TEMP_DESTINATION"
mv -f "$TEMP_DESTINATION" "$DESTINATION"
trap 'rm -rf "$TEMP_DIR"' EXIT

echo "已安装：$DESTINATION"

case ":$PATH:" in
  *":$INSTALL_DIR:"*)
    echo "现在可以运行：danmu <房间号>"
    ;;
  *)
    cat <<EOF

$INSTALL_DIR 当前不在 PATH 中。请将下面一行加入 ~/.zshrc：

  export PATH="$INSTALL_DIR:\$PATH"

然后执行：

  source ~/.zshrc
  danmu <房间号>
EOF
    ;;
esac
