# Workflows

Sometimes you want more structure than "let the main agent decide what to do next." Maybe you want a repeatable pipeline where a **planner** writes a design first, two **implementers** code and write tests in parallel, and a separate **reviewer** signs off at the end.

In Hya, that pipeline is called a **Workflow**: a simple Markdown file (`*.hya.md`) with a list of steps (*Stages*) and a small flowchart showing which step runs after which.

---

## Where Hya looks for Workflows

Hya discovers Workflows automatically from three places (top wins when names match):

1. **Your Project:** `<project-root>/.hya/workflows/*.hya.md`
2. **Your User folder:** `~/.config/hya/workflows/*.hya.md`
3. **Installed & built-in Bundles:** such as the built-in `plan-impl-review` workflow shipped in `hya/workflow` or any `.hyabundle` you install.

> **Built-in bundle (`hya/workflow`):** Workflow discovery, DAG execution, durable event replay, the `workflow` tool, and the v2 Bundle API endpoints (`GET /v2/bundles/hya%2Fworkflow/api/workflows`, `GET /v2/sessions/{id}/bundles/hya%2Fworkflow/state`, `POST /v2/sessions/{id}/bundles/hya%2Fworkflow/command`) are provided by the first-party ABI v2 native bundle `hya/workflow` (`bundles/builtin/workflow/`). In `--bare` mode or when `hya/workflow` is disabled under `bundles.builtin.disable` in `config.yaml`, workflow commands return an unavailable error cleanly without affecting core sessions.

---

## What a Workflow file looks like

Here is a complete Workflow you can drop into `.hya/workflows/ship-fix.hya.md`:

```markdown
---
kind: Workflow
name: ship-fix
description: Plan a bug fix, implement it, and review the diff.
inputs:
  bug: Short description of the bug to fix.
nodes:
  plan:
    title: Investigate & plan
    agent: hya-scout
    directive: Investigate {{input.bug}} and write a concrete fix plan.
  implement:
    title: Apply fix & tests
    agent: hya-task
    directive: Implement the plan from upstream and run the relevant tests.
  review:
    title: Review patch
    agent: hya-reviewer
    directive: Review the changes against {{input.bug}} and check for regressions.
---
flowchart TD
  plan --> implement
  implement --> review
```

When `plan` finishes, Hya automatically passes its output forward into `implement`, and then passes `implement`'s output into `review`. Steps linked with `&` (like `plan --> code & tests`) run **in parallel**!

---

## Running Workflows in the TUI (`/workflow`)

Inside `hya`, use `/workflow` (or `/workflows` to browse):

```text
/workflow list
/workflow info ship-fix
/workflow select ship-fix
/workflow run bug="Login button stays disabled after password reset"
/workflow state
```

While a Workflow is running, your session tracks each Stage's progress live, and you can inspect every Stage's helper session in `/subagents` or `/sessions`. Press **`Esc`** anytime if you need to cancel the run cleanly.

---

## Running Workflows from the terminal (`hya workflow`)

You can list, inspect, and trigger Workflows straight from the command line or CI scripts:

```sh
# List every Workflow available in the current folder
hya workflow list

# Inspect a Workflow's stages and required inputs
hya workflow info ship-fix

# Run a Workflow in a fresh session (add --db ./hya.db to save the run on disk)
hya workflow run ship-fix --input bug="Login button stays disabled"
```

Add `--json` to `hya workflow run` if you want structured JSON results for a script.

---

## Try the full Argus example

Prebuilt releases include a rich multi-stage example bundle called **Argus** (`examples/hya-argus-example.hyabundle` inside your install folder, e.g. `~/.local/examples/hya-argus-example.hyabundle`) that demonstrates parallel investigation, implementation, dual architecture/quality review, and verification:

```sh
hya bundle install ~/.local/examples/hya-argus-example.hyabundle
hya bundle info hya/argus-example
hya workflow run argus --input request="Add retry backoff to the webhook client"
```

Want to write advanced Workflows with **loop verifiers**, **per-stage model & reasoning overrides**, or package them into `.hyabundle` files? Head over to **[Authoring Workflows](../extend/authoring-workflows.md)**.
