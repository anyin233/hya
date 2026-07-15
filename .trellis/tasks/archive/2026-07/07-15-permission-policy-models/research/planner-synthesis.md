# Planner Synthesis

## Consensus

- Keep wildcard `PermissionRules` unchanged and add a compiled native invocation-policy layer in `hya-tool`.
- Classify targets explicitly through tool/registry metadata; do not infer MCP solely from its name prefix.
- Authorize once after hooks and successful lookup, before execution, for both model tool calls and direct shell calls.
- Reuse the existing ask/interceptor/event path and attach message/tool-call correlation.
- Carry validated policy through `FileConfig -> ResolvedConfig -> RuntimeConfig -> build_session_engine`.
- Use call-scoped authorization to suppress only a duplicate primary internal ask while retaining explicit wildcard denies and the separate external-directory boundary.
- Remove normal headless auto-approval; `--yolo` selects effective `danger` before engine construction.
- Cover evaluator, config, dispatch, headless, compatibility, docs, version, and changelog with ordered TDD slices.

## Reconciled Differences

| Topic | Proposals | Resolution |
| --- | --- | --- |
| MCP classification | Name-prefix inference vs explicit metadata | Use explicit metadata; prefix inference lets a plugin cross domains. |
| Interaction/control defaults | Some planners left implicit | Treat every unmatched non-read-only/non-task tool as `Ask`, per the selected default matrix. |
| `danger` vs legacy denies | Preserve denies vs bypass all permission results | Follow the user-selected unconditional `danger`; it bypasses configured and legacy decisions. |
| Invalid config fallback | Existing default/offline fallback vs strict fallback | Preserve the existing error-reporting path but use strict permission fallback to avoid a security downgrade. |
| Permission-only config | Treat as empty vs meaningful | A nonempty rule list or non-default model is meaningful; omitted/default-empty policy remains the normal default. |

## Final Decision

- New invocation-policy `AllowAlways` remembers exact `(target, value)` pairs. Strict asks on first use of each distinct target; explicit denies remain authoritative. Existing wildcard resource approvals remain action-wide.
- Default-read metadata covers local reads only: `read`, `ls`, `glob`, `find`, `grep`, `lsp`, `skill`, `list_agents`, `roster`, and `channels`. Network reads remain ask-by-default.

## Provisional Minimal Shape

- `hya-tool::permission`: model/target/compiled regex policy, one shared ask-resolution helper, and call-scoped primary authorization.
- `Tool`/registry: explicit target metadata and default-read-only classification; plugin registration defaults to normal `tool`, MCP registration marks `mcp`, shell marks `command`.
- `hya-core`: one authorization helper shared by `engine/turn.rs` and `engine/shell.rs`.
- `hya-app`: YAML DTOs, compilation/validation, runtime propagation, strict invalid-config fallback, starter config, and headless behavior.
- `hya-server`/`hya-plugin`/TUI mappings: additive generic tool action/resource presentation only if required by the final ask identity.
