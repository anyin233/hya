Project: /Users/saber/Projects/hya
Phase: done
Step: record
Outcome: commit 2feda313 pushed to origin/main; unrelated hya-core edits remain unstaged

# Shift+Enter composer newline

| ID | Task | Depends on | Status | Owner | Acceptance |
|---|---|---|---|---|---|
| 1 | Locate key path and add failing browser regression | — | done | coordinator | Shift+Enter fails for missing newline behavior |
| 2 | Implement newline behavior | 1 | done | coordinator | Multiline draft stays unsent; Enter sends once; resize/exit intact |
| 3 | Document, bump version, verify and commit/push | 2 | done | coordinator | Frontend gates, binary build and visual proof pass; atomic commit pushed |
