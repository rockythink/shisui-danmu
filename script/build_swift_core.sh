#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
usage() {
  echo "usage: $0 --configuration debug|release --arch arm64|universal --output PATH [--test-support]" >&2
  exit 2
}
CONFIGURATION=
ARCH=
OUTPUT=
TEST_SUPPORT=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --configuration|--arch|--output)
      [[ $# -ge 2 && -n "$2" ]] || usage
      case "$1" in
        --configuration) CONFIGURATION="$2" ;;
        --arch) ARCH="$2" ;;
        --output) OUTPUT="$2" ;;
      esac
      shift 2 ;;
    --test-support) TEST_SUPPORT=(--test-support); shift ;;
    *) usage ;;
  esac
done
[[ "$CONFIGURATION" == debug || "$CONFIGURATION" == release ]] || usage
[[ "$ARCH" == arm64 || "$ARCH" == universal ]] || usage
[[ -n "$OUTPUT" ]] || usage
[[ "$CONFIGURATION" != release || ${#TEST_SUPPORT[@]} -eq 0 ]] || { echo "test-support is forbidden in release" >&2; exit 2; }
command -v python3 >/dev/null || { echo "Python 3.11+ is required" >&2; exit 1; }
exec python3 "$ROOT_DIR/script/swift_core_package.py" build --source "$ROOT_DIR" \
  --configuration "$CONFIGURATION" --arch "$ARCH" --output "$OUTPUT" ${TEST_SUPPORT[@]+"${TEST_SUPPORT[@]}"}
