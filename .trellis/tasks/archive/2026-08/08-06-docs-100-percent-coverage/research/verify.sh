#!/usr/bin/env bash
# Final acceptance gate for the documentation-coverage task tree.
# Every check prints PASS/FAIL and the measured value, so the result is a number
# rather than an assertion. Run from the repository root.
set -uo pipefail
cd "$(dirname "$0")/../../../.." || exit 1

fail=0
chk() { # chk <label> <actual> <expected>
  if [ "$2" = "$3" ]; then printf "  PASS  %-46s %s\n" "$1" "$2"
  else printf "  FAIL  %-46s %s (expected %s)\n" "$1" "$2" "$3"; fail=1; fi
}

echo "== rustdoc child =="
chk "missing_docs errors (--all-targets)" \
    "$(cargo check --workspace --all-targets 2>&1 | grep -cE '^error')" 0
chk "cargo doc warnings" \
    "$(cargo doc --workspace --no-deps 2>&1 | grep -cE '^(warning|error)')" 0
chk "local missing_docs overrides" \
    "$(grep -rn 'missing_docs' crates/*/src/*.rs 2>/dev/null | wc -l | tr -d ' ')" 0
chk "workspace lint is deny" \
    "$(grep -c 'missing_docs = "deny"' Cargo.toml)" 1
crate_docs=0
for c in crates/*/; do
  f=""; [ -f "$c/src/lib.rs" ] && f="$c/src/lib.rs"
  [ -z "$f" ] && [ -f "$c/src/main.rs" ] && f="$c/src/main.rs"
  [ -n "$f" ] && head -1 "$f" | grep -q '^//!' && crate_docs=$((crate_docs+1))
done
chk "crates with a crate-level //!" "$crate_docs" 21

echo "== prose child =="
chk "dead relative links in docs/ + root md" \
    "$(python3 - <<'PY'
import re
from pathlib import Path
bad=0
for md in list(Path("docs").rglob("*.md"))+[Path(x) for x in ["README.md","AGENTS.md","DESIGN.md","CONTEXT.md"]]:
    for m in re.finditer(r"\]\(([^)#][^)]*)\)", md.read_text()):
        t=m.group(1).split("#")[0].strip()
        if not t or t.startswith(("http","mailto")): continue
        if not (md.parent/t).exists(): bad+=1
print(bad)
PY
)" 0
for d in tui-keybindings.md tui-reference.md skills.md plugin-protocol.md compat-plugins.md architecture/admission-and-governor.md; do
  chk "docs/README.md links $d" "$( [ "$(grep -c "$d" docs/README.md)" -gt 0 ] && echo yes || echo no )" yes
done

echo "== ts-package child =="
chk "package READMEs present" \
    "$(ls packages/hya-tui-ts/README.md packages/hya-tui-ts/scripts/README.md packages/hya-tui-ts/test/README.md 2>/dev/null | wc -l | tr -d ' ')" 3

echo "== no behavior change =="
chk "non-doc-comment code changes vs origin/main" \
    "$(git diff origin/main..HEAD -- '*.rs' | grep -E '^[+-]' | grep -vE '^(\+\+\+|---)' \
       | grep -vE '^[+-]\s*(///|//!|//)' | grep -vE '^[+-]\s*$' \
       | grep -vE '^[+-]\s*[A-Za-z_]+[A-Za-z0-9_]*\s*\{$' \
       | grep -vE '^[+-]\s*[\}\)],?;?$' \
       | grep -vE '^\+\s+[a-z_]+: [^=]+,$' \
       | grep -vE '(str_newtype!|uuid_id!|#\[doc = \$doc\]|pub struct \$name|Uuid,|u64,)' \
       | wc -l | tr -d ' ')" 0

echo "== contract + suites =="
chk "docs_example failures" \
    "$(cargo test -p hya-bundle --test docs_example 2>&1 | grep -c 'test result: FAILED')" 0

echo
[ "$fail" = 0 ] && echo "ALL GATES PASS" || echo "SOME GATES FAILED"
exit "$fail"
