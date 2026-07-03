#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

contains() {
  local haystack=$1
  local needle=$2
  [[ "$haystack" == *"$needle"* ]] || fail "expected output to contain: $needle"
}

repo=$(pwd -P)
expected_config_dir="$repo/.claude"
expected_skills_dir="$expected_config_dir/skills"

[[ -x ./scripts/claude-isolated ]] || fail "scripts/claude-isolated must be executable"

root_profile=$(./scripts/claude-isolated --print-isolated-profile)
subdir_profile=$(cd tests && ../scripts/claude-isolated --print-isolated-profile)

[[ "$root_profile" == "$subdir_profile" ]] || fail "isolated profile output changed with cwd"
contains "$root_profile" "repo_root=$repo"
contains "$root_profile" "CLAUDE_CONFIG_DIR=$expected_config_dir"
contains "$root_profile" "skills_dir=$expected_skills_dir"
