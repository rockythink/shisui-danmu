#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_DIR="${DANMU_INSTALL_DIR:-$HOME/.local/bin}"
COMMAND_NAME="danmu"
DESTINATION="$INSTALL_DIR/$COMMAND_NAME"

usage() {
  cat <<'EOF'
用法: ./script/install_cli.sh [--uninstall]

默认将 Release 版本安装到 ~/.local/bin/danmu。
可通过 DANMU_INSTALL_DIR 指定其他用户可写目录：

  DANMU_INSTALL_DIR="$HOME/bin" ./script/install_cli.sh
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

cd "$ROOT_DIR"
echo "正在构建 danmu Release 版本…"
swift build -c release --product ShisuiDanmuTerminal
BUILD_DIR="$(swift build -c release --show-bin-path)"
SOURCE="$BUILD_DIR/ShisuiDanmuTerminal"

mkdir -p "$INSTALL_DIR"
TEMP_DESTINATION="$DESTINATION.tmp.$$"
trap 'rm -f "$TEMP_DESTINATION"' EXIT
cp "$SOURCE" "$TEMP_DESTINATION"
chmod 0755 "$TEMP_DESTINATION"
mv -f "$TEMP_DESTINATION" "$DESTINATION"
trap - EXIT

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
