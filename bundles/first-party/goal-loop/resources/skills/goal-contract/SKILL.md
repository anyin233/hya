---
name: goal-contract
description: Use when authoring or reviewing a structured goal condition (the five `##` sections) for hya goal mode, or when writing the rubric a `goal.evaluate` plugin applies.
---

# Goal condition contract

A structured goal condition uses exactly these five `##` sections, in order:

1. `## Objective` — one sentence, no success language inside it.
2. `## Success criteria` — binary, deterministic statements only. Reject
   "works well", "clean", "done": every criterion must be checkable without
   judgment.
3. `## Verification` — the exact commands that decide each criterion (fenced
   code block, backtick-quoted command, or a `$ ` line). A criterion without a
   command is not a criterion.
4. `## Boundaries` — scope edges and a denylist of forbidden actions/paths.
5. `## Stop conditions` — explicit stop and escalation triggers, including the
   attempt cap. "Until it works" is not a stop condition.

Re-ask rules when interviewing an author: a vague done with no checkable
signal; uncapped iteration; self-graded success with no verification command.

# Evaluator rubric (for `goal.evaluate` providers)

Audit the transcript against the objective's deliverables, in this order:

1. Map each success criterion to current-repo evidence. Never rely on
   earlier-session memory — the repo may have changed.
2. Verification scope equals claim scope: a narrow check does not prove a
   broad claim.
3. Uncertainty is not achievement: if evidence is ambiguous, `met=false`.
4. Never redefine success as a smaller or already-completed subset.
5. Budget exhaustion is not completion.
6. A boundary violation or triggered stop condition prevents achievement even
   when the requested deliverable appears complete.
