#!/usr/bin/env bash
# Local equivalent of the TUI CI job; generated artifacts live under ~/data.
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
check_dir="${HYA_TUI_CHECK_DIR:-$HOME/data/hya-tui-check}"
mkdir -p "$check_dir/tmp"
export TMPDIR="$check_dir/tmp"
export PLAYWRIGHT_HTML_OUTPUT_DIR="$check_dir/report"
export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-$check_dir/browsers}"
export BUN_INSTALL_CACHE_DIR="${BUN_INSTALL_CACHE_DIR:-$check_dir/bun-cache}"

for package in packages/hya-tui packages/hya-tui-web crates/hya-plugin-bun/adapter packages/hya-tui-sdk; do
  (
    cd "$repo_root/$package"
    bun install --frozen-lockfile
    bun run typecheck
    if [[ "$package" == packages/hya-tui-web ]]; then
      bun test ./test
    else
      bun test
    fi
  )
done

cd "$repo_root"
cargo build --locked -p hya-backend --bin hya -p xtask --bin xtask
export HYA_BIN="$repo_root/target/debug/hya"
cd packages/hya-tui-web
bunx playwright install chromium
bun run test:e2e --workers "${HYA_TUI_TEST_WORKERS:-2}" --output "$check_dir/results"
