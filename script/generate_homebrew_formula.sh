#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1#v}"
ARTIFACTS_DIR="${2:-$ROOT_DIR/dist}"
OUTPUT="${3:-$ROOT_DIR/dist/homebrew/danmu.rb}"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "用法：$0 <版本> [Release 制品目录] [输出文件]" >&2
  exit 1
fi

checksum() {
  local file="$ARTIFACTS_DIR/$1.sha256"
  [[ -f "$file" ]] || { echo "缺少校验文件：$file" >&2; exit 1; }
  cut -d ' ' -f 1 < "$file"
}

MACOS_ARM_SHA="$(checksum shisui-danmu-macos-aarch64.tar.gz)"
MACOS_X64_SHA="$(checksum shisui-danmu-macos-x86_64.tar.gz)"
LINUX_ARM_SHA="$(checksum shisui-danmu-linux-aarch64.tar.gz)"
LINUX_X64_SHA="$(checksum shisui-danmu-linux-x86_64.tar.gz)"
mkdir -p "$(dirname "$OUTPUT")"

cat > "$OUTPUT" <<FORMULA
class Danmu < Formula
  desc "Live interaction console for knowledge streamers"
  homepage "https://github.com/rockythink/shisui-danmu"
  version "$VERSION"
  license "MPL-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/rockythink/shisui-danmu/releases/download/v$VERSION/shisui-danmu-macos-aarch64.tar.gz"
      sha256 "$MACOS_ARM_SHA"
    else
      url "https://github.com/rockythink/shisui-danmu/releases/download/v$VERSION/shisui-danmu-macos-x86_64.tar.gz"
      sha256 "$MACOS_X64_SHA"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/rockythink/shisui-danmu/releases/download/v$VERSION/shisui-danmu-linux-aarch64.tar.gz"
      sha256 "$LINUX_ARM_SHA"
    else
      url "https://github.com/rockythink/shisui-danmu/releases/download/v$VERSION/shisui-danmu-linux-x86_64.tar.gz"
      sha256 "$LINUX_X64_SHA"
    end
  end

  def install
    bin.install "danmu"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/danmu --version")
  end
end
FORMULA

printf '已生成 Homebrew Formula：%s\n' "$OUTPUT"
