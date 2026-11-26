# {plan_id} — <outcome>

## Goal and scope

<Who needs what outcome? Give one observable end-to-end acceptance example.
Name any boundary that affects the work.>

## Kanban

| ID | Deliverable | Depends on | Status | Acceptance check / evidence |
| --- | --- | --- | --- | --- |
| T1 | <first independently verifiable change> | — | Todo | <command or observable outcome> |
| T2 | <next change> | T1 | Todo | <command or observable outcome> |

Use Todo, Doing, Blocked or Done. Pick the first Todo whose dependencies are
Done. When blocked, record what would unblock the row. When done, record the
result or link to evidence. Split a large row into stable IDs such as T2.1 and
T2.2 and update dependencies. This table is the board.

## Decisions and handoff

- <Finding or decision, its reason, and the next action on resume.>

The task IDs, dependencies, decomposition and per-task verification follow
[Task Master](https://github.com/eyaltoledano/claude-task-master).
