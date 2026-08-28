#!/usr/bin/env bash
set -Eeuo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 SOURCE_DIR OUTPUT.hyabundle" >&2
  exit 2
fi

source_dir=$1
output=$2
[[ -d "$source_dir" ]] || { echo "source directory does not exist: $source_dir" >&2; exit 1; }

# The hya-bundle writer is the single package-format authority. It validates
# the exact source closure before emitting deterministic public archive bytes.
repo_root=$(cd "$(dirname "$0")/.." && pwd -P)
cargo run --quiet --manifest-path "$repo_root/Cargo.toml" -p xtask -- \
  package-bundle "$source_dir" "$output"
