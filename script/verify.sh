#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

bash -n script/install_cli.sh script/install_release.sh script/package_release.sh
swift test
swift build -c release --product ShisuiDanmuTerminal
HELP_OUTPUT="$(swift run ShisuiDanmuTerminal --help)"
printf '%s\n' "$HELP_OUTPUT"

for required in --login --logout --configure-obs 'danmu <房间号>'; do
  if ! grep -Fq -- "$required" <<<"$HELP_OUTPUT"; then
    echo "帮助文本缺少：$required" >&2
    exit 1
  fi
done

PACKAGE_DESCRIPTION="$(swift package describe)"
if grep -Fq 'ShisuiDanmuDesk' <<<"$PACKAGE_DESCRIPTION"; then
  echo "公开 package 不得包含 ShisuiDanmuDesk" >&2
  exit 1
fi

echo "公开 TUI 验证通过。"
