# Task Plan

## Goal

Configure `12th-oai/gpt-5.6-sol` and verify every supported subagent capability
through installed `hya-ts` with canonical event evidence.

## Phases

- [complete] Finalize and approve bounded E2E requirements/design.
- [complete] Configure the user model route and validate the installed catalog.
- [blocked] Run the ordered real-PTY subagent matrix and diagnose failures:
  discovery, foreground, resume, parallel/category/inline, and background passed;
  nested stopped on upstream HTTP 524 before its first model step.
- [complete] Verify immediate-stop hygiene and record redacted evidence.
- [pending] Rerun nested, resident/mailbox, and read-only TUI slices against a
  healthy provider, then archive the task.

## Current Gate

Blocked by the approved immediate-stop rule: upstream HTTP 524 in the nested
child after 18 remote calls. A fresh bounded execution window is required for
the unrun resident/mailbox/TUI slices.
