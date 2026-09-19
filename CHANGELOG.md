# 0.36.49

## Dispatch-time model resolution for subagent spawns (core, app, tool)

- The parent agent no longer needs to know concrete model ids. A
  caller-supplied model request on a `task` spawn (member or inline
  overlay) is now resolved automatically at dispatch against the live
  catalog, in three branches:
  1. **Exact valid id** — a full `provider/model` id present in the
     catalog dispatches directly (highest precedence, as before).
  2. **Substring fallback** — an invalid id dispatches the first
     catalog id (stable provider/model sort order) containing it as a
     substring. A bare vendor id (`fake`, `anthropic`, `openai`, ...)
     never substring-dispatches; the branch is disabled for it.
  3. **User configuration** — no request, an empty request, or a
     request neither branch could dispatch defers to the user's
     configured chain (definition policy, remembered preference,
     process default) instead of overriding verbatim.
- New `hya_core::category::resolve_dispatch_model` (pure, unit-tested);
  wired into the app runtime's spawn-member resolution. Workflow
  member routing keeps its explicit assignment semantics.
- The `task` tool schema keeps the optional `model` parameter with a
  neutral description ("resolved automatically against the current
  catalog"); the tool prose no longer advertises passing model ids.
- Verification: 5 resolver unit tests; hya-app spawn-precedence tests
  updated to the new contract (non-dispatchable ids defer, dispatchable
  ids still win at their layer); new e2e scenario T2.17 proves all
  three branches plus the bare-vendor deferral against a real backend
  (exact id -> `pref-target`; substring `target` -> first stable match
  `fake/override-target`; bare `fake` and no model -> remembered
  preference). Matrix: 46 scenarios, 9 retired.
