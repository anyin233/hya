Project: /Users/saber/Projects/hya
Phase: closing
Step: commit
Outcome: implementation verified; final workspace gate is blocked by unrelated concurrent hya-core edits

# Shift+Enter composer newline

| ID | Task | Depends on | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| 1 | Locate key path and add failing browser regression | — | done | coordinator | Shift+Enter fails for missing newline behavior |
| 2 | Implement newline behavior | 1 | done | coordinator | Multiline draft stays unsent; Enter sends once; resize/exit intact |
| 3 | Document, bump version, verify and commit/push | 2 | in_progress | coordinator | Frontend gates, binary build and visual proof pass; atomic commit pushed |
