#!/usr/bin/env bash
# Local dry-run of hya-updater stage + optional owner-gated activation.
# Demo keys only — never use for production signing.
set -euo pipefail

root_dir=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$root_dir"

cargo build -q -p hya-updater --bin hya-updater
bin="${CARGO_TARGET_DIR:-target}/debug/hya-updater"

workdir=$(mktemp -d "${TMPDIR:-/tmp}/hya-self-update-demo.XXXXXX")
trap 'rm -rf "$workdir"' EXIT

updater_root="$workdir/updater"
package_dir="$workdir/package"
mkdir -p "$updater_root" "$package_dir"

# Fixture artifact + smoke script.
printf 'demo-payload-v1\n' >"$package_dir/payload.txt"
cat >"$package_dir/smoke.sh" <<'SMOKE'
#!/bin/sh
echo smoke-ok
SMOKE
chmod +x "$package_dir/smoke.sh"

# Build signed metadata with a tiny Rust one-shot (uses the same crate APIs).
meta_json="$workdir/release.metadata.json"
cargo run -q -p hya-updater --example sign_fixture -- \
  --out "$meta_json" \
  --sequence 1 \
  --platform "$(rustc -vV | sed -n 's/^host: //p')" \
  --artifact payload.txt \
  --artifact smoke.sh \
  --package "$package_dir" \
  --write-roots "$updater_root/trust_roots.json" \
  --key-id demo-ci

platform=$(rustc -vV | sed -n 's/^host: //p')

echo "== stage only =="
"$bin" apply \
  --root "$updater_root" \
  --metadata "$meta_json" \
  --package "$package_dir" \
  --platform "$platform" \
  --smoke smoke.sh

"$bin" status --root "$updater_root"

echo "== discard staged candidate =="
"$bin" discard --root "$updater_root" --sequence 1

echo "== owner-authorized activate =="
"$bin" apply \
  --root "$updater_root" \
  --metadata "$meta_json" \
  --package "$package_dir" \
  --platform "$platform" \
  --smoke smoke.sh \
  --owner-authorized-activation

"$bin" status --root "$updater_root"
echo "demo ok under $updater_root"
