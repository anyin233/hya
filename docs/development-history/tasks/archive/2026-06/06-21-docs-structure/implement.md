# Docs Structure Implementation Plan

## Steps

1. Read current Rust source files that define public behavior and crate
   boundaries.
2. Start the Trellis task after this plan is present.
3. Create `docs/` and `docs/architecture/` pages from the approved structure.
4. Populate user-facing pages from README and CLI source.
5. Populate architecture pages from crate source and tests.
6. Populate `project-structure.md` with a detailed repository map.
7. Add focused cross-links between docs pages.
8. Validate that docs do not mention `.trellis/` or Trellis workflow.
9. Validate relative Markdown links under `docs/`.
10. Run formatting-neutral checks; do not run Rust tests unless code changed.

## Validation Commands

```bash
rg -n "Trellis|\\.trellis" docs
python3 - <<'PY'
from pathlib import Path
import re, sys
root = Path('docs')
bad = []
for path in root.rglob('*.md'):
    text = path.read_text()
    for match in re.finditer(r'\[[^\]]+\]\(([^)]+)\)', text):
        target = match.group(1)
        if '://' in target or target.startswith('#') or target.startswith('mailto:'):
            continue
        file_part = target.split('#', 1)[0]
        if not file_part:
            continue
        resolved = (path.parent / file_part).resolve()
        if not resolved.exists():
            bad.append((path, target))
if bad:
    for path, target in bad:
        print(f'{path}: missing link target {target}')
    sys.exit(1)
print('all relative markdown links resolve')
PY
```
