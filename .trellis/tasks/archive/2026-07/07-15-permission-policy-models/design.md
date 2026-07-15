# Design

## Scope

Add one native invocation-policy decision before a registered tool executes.
The policy selects canonical built-in/plugin names, namespaced MCP names, and
shell command text with ordered regular expressions. Existing wildcard
`PermissionRules` continue to own path, URL, subagent-type, external-directory,
and other resource checks.

This is one cohesive runtime change rather than separate task children: config,
registry metadata, dispatch, asks, and headless behavior must agree on the same
decision contract before any slice is useful.

## Configuration Contract

The native YAML shape is:

```yaml
permission:
  model: default
  rules:
    - target: tool
      selector: "^(read|grep)$"
      permission: Allow
    - target: mcp
      selector: "^mcp__github__"
      permission: Ask
    - target: command
      selector: "^git (status|diff)"
      permission: Allow
```

`model` accepts lowercase `allow`, `default`, `strict`, or `danger`. `target`
accepts lowercase `tool`, `mcp`, or `command`. `permission` accepts `Allow`,
`Deny`, or `Ask`. Omitting the section is equivalent to `model: default` with
an empty rule list.

`hya-app` deserializes this shape through the existing `FileConfig` path and
asks `hya-tool` to compile every selector once. Invalid enum values and regexes
return the existing contextual config error. Because `resolve_runtime` currently
recovers from config errors, its recovery runtime uses `strict` permission
policy so a malformed security policy cannot downgrade to default permissions.

A permission-only config is meaningful even without providers, MCP servers, or
plugins. The runtime keeps that policy while selecting the offline provider.
The starter YAML includes an explicit empty default permission section.

## Invocation Metadata

`ToolRegistry` owns a small permission classification beside each registration:

| Class | Rule subjects | Default fallback |
| --- | --- | --- |
| Local read-only | canonical `tool` name | Allow |
| Task | canonical `tool` name | Allow |
| Standard built-in/plugin | canonical `tool` name | Ask |
| MCP | namespaced `mcp` name | Ask |
| Shell command | canonical `tool` name and command text | Ask |

The local read-only set is exactly `read`, `ls`, `glob`, `find`, `grep`, `lsp`,
`skill`, `list_agents`, `roster`, and `channels`. `webfetch` and `websearch` are
standard tools and therefore ask by default. `task` has its own default-allowed
classification. Plugin registration defaults to standard `tool`; MCP
registration explicitly supplies `mcp`; `shell` and `bash` supply command
metadata.

Classification stays in the registry construction code rather than adding a
method to every `Tool` implementation. Legacy hidden aliases resolve to the
inner canonical name. Model-facing registered names such as `bash` remain their
own canonical names.

## Rule Evaluation

An invocation exposes one or more applicable `(target, value)` subjects:

- A normal built-in or plugin exposes `tool=<canonical name>`.
- An MCP tool exposes only `mcp=<mcp__server__tool>`; it does not also match
  `tool` rules.
- A shell invocation exposes `tool=<shell|bash>` and
  `command=<full command after before-hooks>`.

The evaluator scans configured rules in file order. A shell invocation can
match both tool-name and command-text rules, but the evaluator produces one
decision and at most one prompt.

| Model | Evaluation |
| --- | --- |
| `allow` | Any matching `Deny` denies; otherwise allow. Matching `Allow` and `Ask` do not narrow the mode. |
| `default` | The last matching rule wins. With no match, local read-only and `task` allow; every other invocation asks. |
| `strict` | Any matching `Deny` denies; otherwise use an exact remembered grant or ask. Configured `Allow` and `Ask` do not weaken strict mode. |
| `danger` | Allow immediately and bypass configured and legacy permission decisions. |

For a default-mode `Ask`, the remembered subject is the subject matched by the
winning rule. For an unmatched ask, it is the invocation's primary subject:
command text for shell, namespaced name for MCP, and canonical name for other
tools. Strict uses the same primary subject. An effective deny is checked before
remembered grants, so `AllowAlways` never overrides it.

Regexes use Rust `regex` search semantics. Full-string matching requires anchors
in configuration; no implicit anchoring or wildcard conversion is added.

## Permission Plane Composition

`PermissionPlane` receives the compiled invocation policy in addition to its
unchanged snapshot and persistent wildcard rules. It owns an in-memory set of
exact native grants keyed by `(target, value)`.

Before execution, `authorize(invocation)`:

1. Evaluates the compiled model and ordered rules.
2. Returns the existing typed denial for `Deny`.
3. Returns a call-scoped permission-plane clone for `Allow`.
4. For `Ask`, checks the exact native grant, then the existing plugin
   interceptor, then sends the existing `AskRequest` and awaits its reply.
5. Stores only the selected exact subject for native `AllowAlways`.

Native asks carry an exact remember scope. Legacy action/resource asks keep
their current action-wide `AllowAlways` behavior. Pending permission handling
uses that scope when coalescing related asks and recording saved permission
metadata: exact native replies affect only the same action/resource, while
legacy replies retain their current action-wide behavior.

The authorized clone marks only the current tool call. Subsequent internal
`PermissionPlane::assert` calls follow this order:

1. `danger` allows immediately.
2. A snapshot `Deny` denies and a snapshot `Allow` allows.
3. The call-scoped grant suppresses an otherwise duplicate `Ask`, except for
   `ExternalDirectory`.
4. Uncovered checks continue through legacy remembered rules, the interceptor,
   and the ask channel exactly as today.

This permits one approved invocation without weakening explicit resource denies
or the separate external-directory trust boundary. No existing wildcard syntax
or precedence changes.

## Dispatch Flow

Both model tool rounds and direct shell execution use this order:

```text
before-hook -> registry lookup -> invocation construction -> native authorize
            -> ToolCtx with call-scoped plane -> tool execute -> after-hook
            -> ToolResult or ToolError event
```

Lookup precedes authorization, so unknown tools fail without a permission
prompt. Shell command extraction uses the post-hook input and validates that
`command` is a string before asking. The permission plane is scoped with session,
message, and tool-call IDs before authorization, preserving existing event and
API correlation. Permission failures remain unrewritable by after-hooks.

## Runtime Surfaces

- Interactive TUI and non-yolo server modes continue forwarding asks to their
  existing permission endpoints and views.
- Headless `exec`, RPC, and goal modes reply `Reject` to residual asks instead
  of silently approving them. Configured/default allows still run normally.
- `--yolo` replaces the effective compiled model with `danger` before engine
  construction. It no longer depends on an auto-allow responder to make checks
  pass.
- Replay-only commands use the default policy because they execute no tools.

Adding the generic native tool action/resource requires exhaustive mapping in
Rust TUI, server compatibility views, and the plugin permission bridge. Wire
changes are additive; existing action/resource values and endpoint shapes stay
valid.

## Compatibility And Non-Goals

- Existing YAML without `permission` keeps the new documented default behavior.
- Existing `PermissionRules`, Compat agent wildcard rules, external-directory
  grants, MCP setup rules, and legacy action-wide approvals are not converted to
  regexes.
- No hot reload, per-agent policy, persisted regex compilation, rule migration,
  or new permission endpoint is added.
- No prefix-based MCP inference is used; registration metadata owns the domain.

## Failure And Rollback

Malformed permission YAML or regexes use the existing config-error message and
strict permission fallback. A closed ask channel returns the existing
`PermissionError::Unavailable`; headless responders reject while alive so they
do not hang.

The feature is additive to config and stores no migrated policy data. Rollback
is the exact source/doc/version file set; old configs remain parseable after a
rollback because the section was previously unknown only to the newer binary.
