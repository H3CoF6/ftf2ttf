#!/usr/bin/env bash
# 安装本地的 ots-sanitize（OpenType Sanitizer），用于在提交前/写代码时预检字体，
# 不必每次都等浏览器加载失败再回来改。
#
# 安装到项目内的 .venv-ots/，不污染全局环境。
set -euo pipefail

cd "$(dirname "$0")/.."

VENV="${OTS_VENV:-.venv-ots}"

if ! command -v python3 >/dev/null 2>&1; then
    echo "error: python3 not found" >&2
    exit 1
fi

python3 -m venv "$VENV"
"$VENV/bin/pip" install --quiet --upgrade pip
"$VENV/bin/pip" install --quiet opentype-sanitizer

BIN="$(find "$VENV" -type f -name 'ots-sanitize' | head -n1)"
if [ -z "$BIN" ]; then
    echo "error: ots-sanitize was not installed" >&2
    exit 1
fi

echo "ots-sanitize installed: $BIN"
echo
echo "Now you can run:"
echo "  cargo test --test ots"
echo "  cargo run -- <font.ttf> --color --check-ots"
