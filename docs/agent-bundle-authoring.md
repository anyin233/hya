# AgentBundle Authoring (0.34.10)

Use this guide to author and install a public-static `AgentBundle`. The complete
single-file example is [`examples/bundle.hya.md`](examples/bundle.hya.md).

## Package and install the example

Copy the example into an otherwise empty directory, enter that directory, and
create the package with an external `7z` program:

```sh
7z a -t7z -mx=0 -ms=off example.hyabundle bundle.hya.md
hya bundle info -f example.hyabundle
hya bundle install example.hyabundle
hya bundle list
hya bundle info hya/docs-example
hya bundle uninstall hya/docs-example
```

Run `info -f` before installation to inspect without changing the registry or
published catalog. The authoring command deliberately creates an unencrypted,
non-solid archive with no compression. A public package must contain exactly
one `bundle.hya.md` at the archive root and no other entry.

The external `7z` program is author tooling only. The hya runtime never shells
out to, locates, or depends on a system `7z`; it uses its strict in-process
reader. Package paths must have the exact lowercase `.hyabundle` suffix, while
the bytes magic remains authoritative for deciding whether the package is
public or private.

## Source forms

The v1 preparer accepts exactly one of:

- `bundle.yaml` multi-file directory sources (prompts, static skill content, …)
- a single `bundle.hya.md` with YAML frontmatter and Markdown body as the sole agent prompt

Both require the exact markers:

```yaml
api_version: hya.agent-bundle/v1
kind: AgentBundle
```

An installable public-static package uses the single-file form.
`bundle.hya.md` must declare exactly one agent and must not set `prompt:` in
frontmatter; the Markdown body is the prepared static prompt.

## Stable AgentName bytes

`stable_id` becomes the public `AgentName`. Those bytes are identity for events,
projection, replay, and spawn resolution. Treat them as stable wire values: do not
rename casually, and do not rely on `local_id` outside the bundle.

## Role vs spawn lifecycle

- `role` (`main` | `subagent`) controls **only** TUI direct-selector visibility:
  `main` is selectable in the TUI; `subagent` is hidden from direct TUI selection.
  Role does not grant spawn authority and does not place subagents into a
  separate TUI selector slot.
- `spawn_lifecycle` (`transient` | `resident`) is orthogonal: it only describes
  how Harness should spawn the entry when spawn is allowed. It does not change
  TUI direct-selector visibility.

## `can_spawn` reachability

`can_spawn` is the caller-facing reachability set. Agent-facing/internal roster
and ordinary spawn are derived only from the current caller's `can_spawn`
reachability, never from `role`. Omitted targets are not reachable; unknown or
denied targets fail closed. Bundles cannot invent reachability past the prepared
catalog.

## `harness_access` vs `resource_view`

- `harness_access`: `none` | `basic` | `full` — which Harness-owned resources enter
  the candidate set.
- `resource_view` (`allow` / `deny` / `aliases` / `namespace`) narrows and renames
  within that candidate set. Deny wins over allow.

A Bundle cannot expand `PermissionPlane` or plugin authority. Effective access is
the narrowing intersection of access, view, and Harness policy.

## Shipped static boundary

Release 0.34.10 installs public packages containing exactly one static agent
definition and its Markdown prompt. The strict installable profile admits no
external static-skill file. Documentation paths, including `docs/examples`,
are not runtime-scanned. External tool, MCP, hook, JavaScript, or Rust execution
references are rejected with typed
`UNSUPPORTED_BUNDLE_FEATURE`; installation does not create a runner or a new
permission plane. There is no sandbox or malicious-code isolation claim.

The exact-one-entry public example contains only its agent declaration and
Markdown prompt; do not add an external static-skill file to that archive.
Built-in or otherwise prepared catalogs may still contain static skill IDs and
content, and `info` can report those IDs.

Private packages are inspection-only. Their metadata is reported exactly as
`authentication=unverified`, `payload=opaque`, and
`activation unsupported-in-0.34.10`. Structural and declared-digest checks do
not establish publisher authenticity.

## Built-ins and legacy

- Built-ins are prepared at build time from repo-native sources under
  `bundles/builtin/`, embedded, merged read-only with installed packages, and
  immutable through bundle commands. They are not discovered at runtime from
  disk.
- Legacy agent files (for example `.hya/agents/*.md` and former compat agent-file
  loaders) are **unsupported**. There is no migration, adapter, or dual catalog.

## Skills and unsupported features

- Built-in or otherwise prepared catalogs may carry bundle-local **static**
  skill content. The strict installable profile does not accept a static-skill
  file, and this catalog support is not a general skill plane, hot installer,
  or remote skill loader.
- Installing the same identity and digest is idempotent. Replacement and
  removal are atomic registry operations.
- Installed catalog changes become visible lazily before a new root turn binds
  and on TUI/catalog refresh. In-flight and child turns remain pinned to the
  catalog snapshot they already hold.

## Example

See [`examples/bundle.hya.md`](examples/bundle.hya.md) for one flat `main` /
`transient` agent with no executable references. Package the directory that
contains it using the exact workflow above; the docs tree itself is never an
installation source.
