#!/usr/bin/env python3
"""Build one Grok fix brief per document from the independent re-audit findings.

Three kinds of finding, in priority order:
  new     -- a claim the NEW writing introduced that the source contradicts.
             Highest priority: the document actively misleads a reader today.
  open    -- an original gap the writers did not actually close.
  critic  -- something no gap entry covered, found by a fresh reader.
"""
import json
from pathlib import Path

HERE = Path(__file__).parent
FIX = json.load(open(HERE / "fixes.json"))

# batch -> files (file-disjoint, as before)
BATCHES = {
    "J1": ["docs/configuration.md", "docs/cli.md"],
    "J2": ["docs/tui-reference.md", "docs/tui-keybindings.md"],
    "J3": ["docs/architecture/runtime.md", "docs/architecture/storage.md",
           "docs/architecture/providers.md", "docs/architecture/server-client.md",
           "docs/architecture/agent-tool-surface.md",
           "docs/architecture/tools-and-permissions.md"],
    "J4": ["docs/compat-plugins.md", "docs/agent-bundle-authoring.md",
           "docs/self-update.md", "docs/compat-parity.md",
           "docs/testing/agent-matrix.md", "docs/testing/process-e2e.md"],
}

HEAD = """# Fix batch {b} - {label}

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

{filelist}

Do not create or edit any other file.

## What the three kinds of finding mean

- **CONTRADICTION** - the document says something the source does not support. The
  new writing introduced it. This is the worst kind: a reader trusts it today.
  Fix by correcting or DELETING the claim. Never leave the wrong text alongside a
  correction.
- **STILL OPEN** - an original gap the previous writer did not really close.
  Usually "thin": the feature is named but a reader still could not use it.
- **CRITIC** - something no gap entry covered, found by a fresh reader.

## Non-negotiable rules

1. **Open the cited source before you change anything.** Every finding names a
   `file:line`. The auditor may itself be wrong - if the source supports the
   current documentation, KEEP it and say so in your report. Do not "fix" correct
   text because a report told you to.
2. Deleting an unsupported claim is a valid and often correct fix. Do not invent
   replacement behaviour to fill the space.
3. Do not weaken precise contract wording into vague prose. Some sentences in
   these documents are asserted verbatim by tests in `crates/hya-bundle/tests/`;
   if you rewrite a sentence that reads like a contract, keep its exact terms.
4. Edit only your files. Other writers are working in parallel.
5. Do not run `git commit`.

## Findings

"""

TAIL = """
## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
"""


def main():
    for b, files in BATCHES.items():
        chunks, n = [], 0
        for f in files:
            data = FIX.get(f)
            if not data:
                continue
            chunks.append("\n### `%s`\n" % f)
            for i, x in enumerate(data["new"], 1):
                n += 1
                chunks.append(
                    "**CONTRADICTION %d**\n\n- The doc claims: %s\n- Reality: %s\n"
                    "- Source: `%s`\n" % (i, x["claim"], x["reality"], x.get("source", "?")))
            for i, x in enumerate(data["open"], 1):
                n += 1
                chunks.append(
                    "**STILL OPEN %d - %s** (`%s`)\n\n- Source: `%s`\n- Why it is still open: %s\n"
                    % (i, x["feature"], x["status"], x.get("source", "?"), x["why"]))
            for i, x in enumerate(data["critic"], 1):
                n += 1
                chunks.append(
                    "**CRITIC %d - %s**\n\n- Source: `%s`\n- Why it matters: %s\n"
                    % (i, x["feature"], x.get("source", "?"), x["why_it_matters"]))
        if not chunks:
            continue
        label = ", ".join(Path(f).name for f in files if FIX.get(f))
        filelist = "\n".join("- `%s`" % f for f in files if FIX.get(f))
        body = HEAD.format(b=b, label=label, filelist=filelist)
        out = HERE / ("fix-%s.md" % b)
        out.write_text(body + "\n".join(chunks) + TAIL)
        print("fix-%s.md  files=%d findings=%d bytes=%d"
              % (b, len([f for f in files if FIX.get(f)]), n, out.stat().st_size))


if __name__ == "__main__":
    main()
