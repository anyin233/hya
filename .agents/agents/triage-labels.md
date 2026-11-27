# Triage Labels

Humans and agents share one triage flow in GitHub Issues. The issue forms in
`.github/ISSUE_TEMPLATE/` (Bug report, Feature request, Task) apply the first
label; agents filing through `gh` apply the same labels and issue type by hand
(see `issue-tracker.md`).

## Labels

| Skill role (mattpocock/skills)       | Our tracker                    | Meaning                                              |
| ------------------------------------ | ------------------------------ | ---------------------------------------------------- |
| `needs-triage`                       | `needs-triage`                 | New or answered; a maintainer must evaluate it        |
| `needs-info`                         | `needs-info`                   | Waiting on the reporter for more information          |
| `ready-for-agent`, `ready-for-human` | `ready-for-develop`            | Fully specified; anyone, human or agent, may take it  |
| `wontfix`                            | close as *not planned*         | Will not be actioned (there is no `wontfix` label)    |

We do not split ready work by who implements it: when a skill names either
ready role, use `ready-for-develop`.

## Flow

1. **Filed**: `needs-triage`, plus the form's issue type (`Bug`, `Feature`,
   `Task`) and type label (`bug`, `enhancement`).
2. **Triage** picks one outcome:
   - Missing information: comment with the questions, swap `needs-triage` for
     `needs-info`. When the reporter answers, swap back to `needs-triage`.
   - Duplicate: label `duplicate`, comment `Duplicate of #<n>`, close.
   - Not doing it: comment the reason, close as not planned.
   - Accepted: remove `needs-triage`, add `ready-for-develop`, one
     `priority: P0`–`P3`, and a type label (`bug`, `enhancement`, `refactor`,
     `chore`, `perf`, `test`, `build`, `ci`, `documentation`).
3. **Ready** means the body has what the Task form asks for: what to build with
   the affected crate/package/path, observable acceptance criteria, and a
   `Blocked by` line. Rewrite a bug or feature request into that shape before
   marking it ready.
4. **Pick up** only `ready-for-develop` issues whose blockers are closed. The
   PR links the issue as the PR template requires.
