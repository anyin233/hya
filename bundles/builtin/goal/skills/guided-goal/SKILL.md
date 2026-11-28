---
name: guided-goal
description: Use when the user wants to turn a rough idea into a goal-mode objective. Conduct a short interview, then emit the five-section goal document.
---

# Guided goal interview

Interview the user in normal conversation: exactly one question per turn, at
most six questions, scoped to this project's real stack, conventions, and
constraints. Pin down all five before creating anything:

1. Binary/deterministic success criteria (reject subjective wording).
2. The verification method — exact commands.
3. An attempt cap (turns, and a token budget when relevant).
4. Scope boundaries plus a denylist.
5. Stop/escalation conditions.

Re-ask when the answer is: vague "done" with no checkable signal; uncapped
iteration ("until CI is green"); self-graded success without a verification
command.

Emit the final objective with exactly these sections, filled with the user's
answers:

```
## Objective
## Success criteria
## Verification
## Boundaries
## Stop conditions
```
