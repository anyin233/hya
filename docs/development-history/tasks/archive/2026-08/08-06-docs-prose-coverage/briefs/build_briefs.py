#!/usr/bin/env python3
"""Build one Grok brief per Wave-1 batch from the RAW audit gap data.

Source of truth is research/raw-gaps.json (324 gap entries + 65 stale claims
recovered from the audit workflow journal), not the synthesized report -- the
report's per-file grouping is lossy and omits gap sections for several files.

Each brief carries only the entries whose target_doc resolves to one of the
batch's own files, so a writer never sees another batch's work list.
"""
import json
import re
from collections import defaultdict
from pathlib import Path

REPO = Path("/chivier-disk/yanweiye/Projects/yaca")
TASK = REPO / ".trellis/tasks"
RAW = TASK / "08-06-docs-100-percent-coverage/research/raw-gaps.json"
OUT = TASK / "08-06-docs-prose-coverage/briefs"

BATCHES = {
    "A": (["docs/configuration.md"],
          "This is the largest single file in the wave (62 gaps). Write only a "
          "POINTER STUB for Skills -- Batch L owns the authoring content -- and only a "
          "POINTER to docs/tui-keybindings.md for slash commands, which already "
          "contains the full table. You DO own the TUI environment-variable reference "
          "table."),
    "B": (["docs/cli.md"],
          "For slash commands write a short list plus a link to "
          "docs/tui-keybindings.md, which already contains the full table."),
    "C": (["docs/architecture/providers.md"], ""),
    "D": (["docs/architecture/runtime.md"], ""),
    "E": (["docs/architecture/event-model.md"],
          "This file documents a WIRE CONTRACT. A wrong claim here misleads "
          "integrators building against the event stream. Verify each Event variant "
          "against the source enum before writing it."),
    "F": (["docs/architecture/storage.md", "docs/architecture/admission-and-governor.md"],
          "admission-and-governor.md is NEW. These two are paired because the "
          "admission schema and the admission API must agree -- you own both so they "
          "cannot drift. This is a safety-critical spawn-budget state machine; prefer "
          "omitting a claim to guessing one."),
    "G": (["docs/architecture/tools-and-permissions.md",
           "docs/architecture/agent-tool-surface.md"],
          "These two files CURRENTLY CONTRADICT EACH OTHER on the write/edit tool "
          "schemas, the builtin tool inventory, and the `skill` row. You own both. "
          "Read the source, decide which is right, and make them agree -- do not "
          "simply pick one. Leave the `skill` row pointing at docs/skills.md, which "
          "Batch L is writing in parallel."),
    "H": (["docs/architecture/server-client.md"], ""),
    "I": (["docs/architecture/tui.md"],
          "This file is ARCHITECTURE only. User-facing TUI behaviour now lives in "
          "docs/tui-reference.md and docs/tui-keybindings.md, which already exist -- "
          "link to them rather than repeating their content."),
    "K": (["docs/plugin-protocol.md", "docs/compat-plugins.md"],
          "Both files are NEW. Paired because the compat adapter implements the "
          "protocol; you own both so the hook vocabulary stays consistent. Do not add "
          "the links to docs/README.md or docs/configuration.md that some entries "
          "mention -- the reconciliation pass and Batch A own those files."),
    "L": (["docs/agent-bundle-authoring.md", "docs/skills.md"],
          "docs/skills.md is NEW and is the canonical home for Skills authoring. "
          "Paired because bundle `resources.skills` overlaps skill discovery. Do not "
          "edit docs/README.md, docs/configuration.md, docs/cli.md, or "
          "docs/self-update.md that some entries mention -- other batches own those."),
    "M": (["docs/self-update.md"], ""),
    "N": (["docs/getting-started.md", "docs/troubleshooting.md", "docs/development.md",
           "docs/testing/process-e2e.md"],
          "Four small user-facing files with no overlap. Do NOT repoint the broken "
          "keybinding link at getting-started.md:171 -- the reconciliation pass owns "
          "cross-links."),
    "P": (["docs/adr/0001-event-sourced-mailbox-and-channels.md",
           "docs/adr/0002-resident-actor-model-and-autonomous-main-agent.md",
           "docs/adr/0003-tmux-tui-single-input-readonly-panes.md",
           "docs/adr/0006-tui-session-reset-and-subagent-visibility.md",
           "CONTEXT.md"],
          "ADRs record decisions as they were made. Correct statements of FACT about "
          "current behaviour, but do not rewrite the historical decision or its "
          "context. If a decision was later reversed, note that rather than deleting "
          "it."),
}

PREAMBLE = """# Batch {b} - {label}

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly {n} file(s). Do not create or edit any other file.

{filelist}

You have **{ngaps} gap entries** and **{nstale} stale claims** to resolve.
{notes}
## Non-negotiable rules

1. **Confirm every claim against the source before you write it.** Every entry
   below carries a `source` reference. Open it. If the source contradicts the
   entry, the SOURCE WINS -- write what the code does and report the discrepancy.
2. **If you cannot confirm a claim from source, do not write it.** Say you could
   not confirm it. Plausible prose that is wrong is worse than an admitted gap,
   because a reader trusts the document.
3. **Stale and contradicted entries are corrected or deleted, never merely
   supplemented.** A document that contradicts the code is a defect.
4. **Do not edit any file outside your batch.** Other writers are working in
   parallel. In particular never touch `docs/README.md`, `README.md`, `AGENTS.md`,
   `DESIGN.md`, or `docs/project-structure.md` -- a later reconciliation pass owns
   all cross-links and the docs map. Some entries below suggest edits to other
   files; ignore that part and write only your own.
5. **Match the existing documentation style.** Read the file you are editing
   before writing. Use the project's vocabulary as defined in `CONTEXT.md`.
6. **A feature counts as documented only if a reader can use it** from what you
   write: what it does, its parameters or keys, and its semantics. A name in a
   list does not count. {nthin} of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

"""

CLOSING = """
## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the {ngaps} gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
"""


PATH_RE = re.compile(r"[\w./-]+\.(?:md|rs|ts|tsx|toml)")


def paths(target):
    """Every file path named in a target_doc / doc string.

    Handles the shapes the audit actually emitted:
      'docs/x.md (new; also link from y)'          -> ['docs/x.md']
      'docs/x.md:412-414'                          -> ['docs/x.md']
      'docs/a.md:169 and README.md:79-80'          -> ['docs/a.md', 'README.md']
      'DESIGN.md sections 2, 5 and 6'              -> ['DESIGN.md']
    The first path is the OWNER; later ones are cross-references that belong to
    whichever batch owns them, so every named path gets the entry and rule 4
    stops a writer from straying into a file it does not own.
    """
    return list(dict.fromkeys(PATH_RE.findall(target)))


def main():
    raw = json.loads(RAW.read_text())
    by_doc = defaultdict(list)
    for g in raw["gaps"]:
        found = paths(g["target_doc"])
        by_doc[found[0] if found else g["target_doc"]].append(g)
    stale_by_doc = defaultdict(list)
    for s in raw["stale"]:
        for p in paths(s["doc"]) or [s["doc"]]:
            stale_by_doc[p].append(s)

    claimed = set()
    OUT.mkdir(parents=True, exist_ok=True)
    for b, (files, notes) in BATCHES.items():
        chunks, ngaps, nstale, nthin = [], 0, 0, 0
        for f in files:
            gs, ss = by_doc.get(f, []), stale_by_doc.get(f, [])
            claimed.add(f)
            ngaps += len(gs)
            nstale += len(ss)
            nthin += sum(1 for g in gs if g["status"] == "thin")
            chunks.append("### `%s`\n" % f)
            if not gs and not ss:
                chunks.append("_No entries. Verify against source and report if it is "
                              "already complete._\n")
            for i, g in enumerate(gs, 1):
                chunks.append(
                    "**%d. %s** — `%s` · severity %s\n\n"
                    "- Source: `%s`\n- Evidence: %s\n- Write: %s\n"
                    % (i, g["feature"], g["status"], g.get("severity", "?"),
                       g["source"], g["evidence"], g["what_to_write"]))
            for i, s in enumerate(ss, 1):
                chunks.append(
                    "**STALE %d.** The document claims: %s\n\n- Reality: %s\n"
                    "- Action: correct or delete. Do not merely supplement.\n"
                    % (i, s["claim"], s["reality"]))
        label = ", ".join(Path(f).name for f in files)
        filelist = "\n".join(
            "- `%s`%s" % (f, "  **(new file)**" if not (REPO / f).exists() else "")
            for f in files)
        body = PREAMBLE.format(b=b, label=label, n=len(files), filelist=filelist,
                               ngaps=ngaps, nstale=nstale, nthin=nthin,
                               notes=("\n" + notes + "\n" if notes else ""))
        (OUT / ("batch-%s.md" % b)).write_text(
            body + "\n".join(chunks) + CLOSING.format(ngaps=ngaps))
        print("batch-%-2s files=%d gaps=%-3d stale=%-2d thin=%-3d bytes=%d"
              % (b, len(files), ngaps, nstale, nthin, len(body) + sum(map(len, chunks))))

    # Anything the batches do not own -- must not be silently dropped.
    orphan_g = {d: len(v) for d, v in by_doc.items() if d not in claimed}
    orphan_s = {d: len(v) for d, v in stale_by_doc.items() if d not in claimed}
    if orphan_g or orphan_s:
        print("\nNOT owned by any Wave-1 batch (reconciliation pass or other child):")
        for d, n in sorted(orphan_g.items(), key=lambda x: -x[1]):
            print("  %3d gaps   %s" % (n, d))
        for d, n in sorted(orphan_s.items(), key=lambda x: -x[1]):
            print("  %3d stale  %s" % (n, d))


if __name__ == "__main__":
    main()
