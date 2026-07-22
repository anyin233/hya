# Agent Tool Surface

This document describes the tools that hya exposes to agents, with detailed
coverage of READ, EDIT, and local file search. It distinguishes three related
but different surfaces:

1. **Registered**: a name resolves in `ToolRegistry`.
2. **Advertised**: a canonical schema is included in a model request or Compat
   tool-list response.
3. **Executable**: the tool has the runtime plane, session, permissions, and
   other resources needed to complete a call.

The distinction matters because aliases resolve but are not advertised,
model filters hide some registered schemas, and several always-
registered builtins delegate to runtime planes that can be disconnected or
empty. The registry stores canonical tools and aliases separately, and only
canonical tools contribute schemas.
([crates/hya-tool/src/tool.rs:107-125](../../crates/hya-tool/src/tool.rs#L107-L125),
[crates/hya-tool/src/tool.rs:203-222](../../crates/hya-tool/src/tool.rs#L203-L222))

## Builtin inventory

`ToolRegistry::builtins()` installs 26 canonical schema names before model
filtering. The inventory below is complete for that constructor.
([crates/hya-tool/src/tool.rs:145-183](../../crates/hya-tool/src/tool.rs#L145-L183))

| Area | Canonical schema names | Role |
| --- | --- | --- |
| File access | `read`, `write`, `edit`, `apply_patch` | Read, replace/write, or patch workspace files. `apply_patch` and `edit`/`write` are advertised mutually exclusively for selected models. |
| Local discovery | `ls`, `glob`, `find`, `grep`, `lsp` | List directories, match paths, search text, or query language servers. |
| Commands | `shell`, `bash` | Two advertised names backed by the same shell implementation. |
| Human/session interaction | `question`, `ask_user`, `todowrite`, `plan_exit`, `invalid` | Ask structured or simple questions, update session todos, request a plan-mode transition, or represent invalid tool arguments. |
| Agents and teams | `skill`, `list_agents`, `task`, `send`, `roster`, `channels`, `join`, `leave` | Load skills, discover/spawn agents, and use team mail/channels. |
| Network | `webfetch`, `websearch` | Fetch a URL or run provider-backed web search. |

The less obvious entries are confirmed by their schemas: `question` accepts a
batch of structured questions, while `ask_user` is a single free-text/select
interaction; `list_agents` enumerates definitions usable by `task`; and the
mailbox tools expose direct/channel send, roster, channel listing, join, and
leave operations.
([crates/hya-tool/src/question.rs:36-78](../../crates/hya-tool/src/question.rs#L36-L78),
[crates/hya-tool/src/tool.rs:693-725](../../crates/hya-tool/src/tool.rs#L693-L725),
[crates/hya-tool/src/agents.rs:70-104](../../crates/hya-tool/src/agents.rs#L70-L104),
[crates/hya-tool/src/mailbox.rs:197-280](../../crates/hya-tool/src/mailbox.rs#L197-L280),
[crates/hya-tool/src/mailbox.rs:340-450](../../crates/hya-tool/src/mailbox.rs#L340-L450))

`task` supports one subagent, a multi-member hya extension, model/category
overrides, session resumption, background execution, resident agents, and
inline ephemeral agent definitions. Execution requires a session and checks
`Action::Task` for every member.
([crates/hya-tool/src/task.rs:103-192](../../crates/hya-tool/src/task.rs#L103-L192),
[crates/hya-tool/src/task.rs:194-277](../../crates/hya-tool/src/task.rs#L194-L277))

`shell` and `bash` have distinct advertised names but wrap the same `ShellTool`.
The schema accepts a command, timeout, working directory, and environment; the
implementation uses `sh -c`, defaults to 120 seconds, and caps returned command
output at 16 KiB while saving the full output to a file.
([crates/hya-tool/src/tool.rs:175-178](../../crates/hya-tool/src/tool.rs#L175-L178),
[crates/hya-tool/src/shell.rs:17-65](../../crates/hya-tool/src/shell.rs#L17-L65),
[crates/hya-tool/src/shell.rs:67-179](../../crates/hya-tool/src/shell.rs#L67-L179))

### Hidden aliases

Five legacy aliases resolve during execution but do not appear in
`ToolRegistry::schemas()`:

| Canonical advertised name | Hidden lookup alias |
| --- | --- |
| `webfetch` | `fetch` |
| `websearch` | `search` |
| `todowrite` | `todo` |
| `apply_patch` | `patch` |
| `plan_exit` | `plan` |

This behavior is explicit in registration and covered by a test that requires
the canonical names to be visible and the aliases to remain hidden.
([crates/hya-tool/src/tool.rs:178-182](../../crates/hya-tool/src/tool.rs#L178-L182),
[crates/hya-tool/tests/tool.rs:111-146](../../crates/hya-tool/tests/tool.rs#L111-L146))

There is therefore no advertised local-file tool named `search`. The hidden
name `search` resolves to **web search**, not GREP. Local discovery is exposed
as `glob`, `find`, and `grep`; provider-backed internet search is advertised as
`websearch` when enabled.
([crates/hya-tool/src/tool.rs:158-160](../../crates/hya-tool/src/tool.rs#L158-L160),
[crates/hya-tool/src/tool.rs:180-181](../../crates/hya-tool/src/tool.rs#L180-L181))

## Advertisement and naming

Before each completion request, hya obtains canonical registry schemas and
applies an advertisement-only filter:

- `use_patch` is true when the model string contains `gpt-`, does not contain
  `oss`, and does not contain `gpt-4`.
- `apply_patch` is advertised only when `use_patch` is true.
- `edit` and `write` are advertised only when `use_patch` is false.
- enabled `websearch` is advertised to every model provider.
- Every other canonical schema passes through.

The tools remain registered even when their schemas are filtered from the
request.
([crates/hya-core/src/engine/turn/messages.rs:38-82](../../crates/hya-core/src/engine/turn/messages.rs#L38-L82))

### Why WEBSEARCH was provider-filtered

The removed `compat` restriction was inherited product policy, not a
model-protocol or tool-execution requirement.

The upstream OpenCode history is explicit. Commit
[`9c237f0`](https://github.com/anomalyco/opencode/commit/9c237f0bfb9335c8ce6c793c4eee0e17ef4d775e)
"temporarily restrict[ed] codesearch and websearch to opencode zen users" while
an enterprise opt-out was unresolved. Commit
[`419983c`](https://github.com/anomalyco/opencode/commit/419983c0f1dcffc4fae28f844e7658326e2ee5aa)
then restored an opt-in for non-Zen users through `OPENCODE_ENABLE_EXA`; its
[pull request](https://github.com/anomalyco/opencode/pull/5132) describes this as
an interim rollout rule. Current OpenCode keeps the same shape: web search is
enabled for its `opencode` provider or when explicit Exa/Parallel flags are set.
([current registry](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/tool/registry.ts),
[Parallel rollout](https://github.com/anomalyco/opencode/pull/26227))

hya commit `fd96760794056ce9eacaad9c6d72768863d890c6` copied the strict
`provider == "opencode"` branch. Commit
`07af114e9284ad3a79c62fa777cecb96a766e91f` later changed that provider string
to `compat` as part of a broad external-compat debranding change. It did not add
the upstream opt-in flags or establish `compat` as a web-search capability.

That distinction matters because hya's provider IDs are user-defined config
keys, and Compat config import preserves those IDs. A provider named `compat`
is therefore neither required nor sufficient to identify OpenCode Zen.
([crates/hya-app/src/config.rs:604-648](../../crates/hya-app/src/config.rs#L604-L648),
[crates/hya-app/src/config.rs:1174-1217](../../crates/hya-app/src/config.rs#L1174-L1217))

The execution path itself never examines the model provider. `tools.websearch`
selects Exa or Parallel, optionally overrides the endpoint and key, and can
disable the built-in. Exa is the enabled, unauthenticated default. Exa keys are
sent as `exaApiKey` query parameters; Parallel keys are sent as bearer tokens.
([crates/hya-tool/src/websearch.rs:23-72](../../crates/hya-tool/src/websearch.rs#L23-L72),
[crates/hya-tool/src/websearch.rs:160-220](../../crates/hya-tool/src/websearch.rs#L160-L220))

The stale compatibility condition was removed. Enabled websearch is now
advertised independently of the model provider.

The OpenAI, Anthropic, and Google request encoders preserve the canonical
`ToolSchema.name`; they translate only the surrounding provider JSON shape.
Descriptions and input schemas are forwarded with the same values.
([crates/hya-provider/src/openai.rs:34-47](../../crates/hya-provider/src/openai.rs#L34-L47),
[crates/hya-provider/src/anthropic.rs:25-35](../../crates/hya-provider/src/anthropic.rs#L25-L35),
[crates/hya-provider/src/google.rs:159-171](../../crates/hya-provider/src/google.rs#L159-L171))

The Compat tool-list implementation sorts canonical schemas by name and
preserves each schema name as the returned `id`. Its separate ID listing is
also sorted; both surfaces now reflect the configured registry without a
websearch model/provider filter.
([crates/hya-server/src/compat/experimental_tool.rs:9-36](../../crates/hya-server/src/compat/experimental_tool.rs#L9-L36),
[crates/hya-server/src/compat/experimental_tool.rs:39-58](../../crates/hya-server/src/compat/experimental_tool.rs#L39-L58))

## READ

### Input schema and path resolution

The advertised schema requires `filePath` and optionally accepts `offset` and
`limit`; it also lists `path` for compatibility. Runtime deserialization stores
the path in one optional field and accepts `filePath` as an alias, so direct
execution accepts either spelling even though provider-side schema validation
can require `filePath`. Both numeric fields advertise a minimum of zero.
([crates/hya-tool/src/read.rs:14-24](../../crates/hya-tool/src/read.rs#L14-L24),
[crates/hya-tool/src/read.rs:28-50](../../crates/hya-tool/src/read.rs#L28-L50))

The workdir is absolutized and lexically normalized. Relative paths are joined
to that workdir; absolute paths remain absolute. Normalization removes `.` and
lexically pops one component for `..`; it does not canonicalize symlinks.
([crates/hya-tool/src/read.rs:52-66](../../crates/hya-tool/src/read.rs#L52-L66),
[crates/hya-tool/src/lsp_path.rs:3-35](../../crates/hya-tool/src/lsp_path.rs#L3-L35))

The external-directory boundary is therefore lexical. As an implementation
consequence, this check does not classify a symlink located inside the workdir
as external based on the symlink target.
([crates/hya-tool/src/read.rs:201-223](../../crates/hya-tool/src/read.rs#L201-L223),
[crates/hya-tool/src/lsp_path.rs:3-35](../../crates/hya-tool/src/lsp_path.rs#L3-L35))

READ performs two permission checks. A path outside the normalized workdir
requires `Action::ExternalDirectory` on the containing directory wildcard (or
the directory itself plus `*`), then every read requires `Action::Read` on the
resolved path. External-directory permission is checked before a missing-path
error is returned.
([crates/hya-tool/src/read.rs:61-74](../../crates/hya-tool/src/read.rs#L61-L74),
[crates/hya-tool/src/read.rs:201-223](../../crates/hya-tool/src/read.rs#L201-L223))

### File-kind dispatch

READ samples the first 4,096 bytes. PNG, JPEG, GIF, WebP, and PDF content is
returned as a base64 data-URL attachment. Detection prefers magic bytes and
falls back to the extension. Other binary files are rejected; the binary
heuristic checks a fixed extension list, NUL bytes, and a greater-than-30%
ratio of selected control bytes in the sample.
([crates/hya-tool/src/read_media.rs:9-27](../../crates/hya-tool/src/read_media.rs#L9-L27),
[crates/hya-tool/src/read_media.rs:29-53](../../crates/hya-tool/src/read_media.rs#L29-L53),
[crates/hya-tool/src/read_media.rs:64-112](../../crates/hya-tool/src/read_media.rs#L64-L112),
[crates/hya-tool/src/read_media.rs:115-165](../../crates/hya-tool/src/read_media.rs#L115-L165))

The attachment result contains `title`, a short `output`, non-truncated
metadata, and one `attachments` entry with `type`, MIME type, and data URL.
Unsupported binary files fail with `Cannot read binary file: <path>` before
UTF-8 decoding.
([crates/hya-tool/src/read.rs:75-97](../../crates/hya-tool/src/read.rs#L75-L97),
[crates/hya-tool/tests/read.rs:215-267](../../crates/hya-tool/tests/read.rs#L215-L267))

### Text limits and output

Text reads are one-based. Omitted or zero `offset` becomes 1, and the default
line limit is 2,000. `limit` is not clamped, so zero collects no lines. An
offset beyond the counted lines is an error. UTF-8 BOM is stripped; remaining
invalid UTF-8 is returned as an I/O invalid-data error.
([crates/hya-tool/src/read.rs:14-14](../../crates/hya-tool/src/read.rs#L14-L14),
[crates/hya-tool/src/read.rs:90-95](../../crates/hya-tool/src/read.rs#L90-L95),
[crates/hya-tool/src/read.rs:225-227](../../crates/hya-tool/src/read.rs#L225-L227),
[crates/hya-tool/src/read_text.rs:22-35](../../crates/hya-tool/src/read_text.rs#L22-L35),
[crates/hya-tool/src/read_text.rs:127-136](../../crates/hya-tool/src/read_text.rs#L127-L136))

Three independent bounds shape text output:

| Bound | Behavior |
| --- | --- |
| Requested/default lines | Stops collecting after `limit`; the default is 2,000. |
| Individual line length | Keeps 2,000 Unicode scalar values and appends `... (line truncated to 2000 chars)`. |
| Aggregate content | Stops before exceeding 50 KiB and tells the caller which `offset` to use next. |

The line and byte implementations are in
[crates/hya-tool/src/read_text.rs:8-12](../../crates/hya-tool/src/read_text.rs#L8-L12)
and
[crates/hya-tool/src/read_text.rs:76-125](../../crates/hya-tool/src/read_text.rs#L76-L125);
tests pin both limits in
[crates/hya-tool/tests/read_limits.rs:56-117](../../crates/hya-tool/tests/read_limits.rs#L56-L117).

The result contains `title`, XML-like `output` with numbered lines, unnumbered
`content`, a 20-line `metadata.preview`, and display metadata for line start,
line end, counted total, and truncation. `metadata.loaded` is currently always
an empty array.
([crates/hya-tool/src/read_text.rs:37-73](../../crates/hya-tool/src/read_text.rs#L37-L73),
[crates/hya-tool/tests/read.rs:60-101](../../crates/hya-tool/tests/read.rs#L60-L101))

Two limit labels need careful interpretation:

- Truncating one overlong line does **not** set `metadata.truncated`; the suffix
  is the only indication.
- Once the 50 KiB loop breaks, `totalLines` is the number scanned through the
  first rejected line, not the actual total number of lines in the file.

Both follow directly from the collector and are pinned by tests that expect an
untruncated flag for a 2,001-character line and `totalLines == 52` for a
60-line file stopped at the byte cap.
([crates/hya-tool/src/read_text.rs:84-113](../../crates/hya-tool/src/read_text.rs#L84-L113),
[crates/hya-tool/tests/read_limits.rs:56-84](../../crates/hya-tool/tests/read_limits.rs#L56-L84),
[crates/hya-tool/tests/read_limits.rs:86-117](../../crates/hya-tool/tests/read_limits.rs#L86-L117))

### Directory reads and missing paths

For a directory, READ lists immediate children only, puts directories first,
sorts each group lexically, applies the same one-based offset and 2,000-entry
default limit, and returns directory-specific display metadata. The 50 KiB text
cap is not applied to this directory result.
([crates/hya-tool/src/read.rs:100-160](../../crates/hya-tool/src/read.rs#L100-L160),
[crates/hya-tool/tests/read.rs:103-140](../../crates/hya-tool/tests/read.rs#L103-L140))

A missing path produces up to three lexically sorted suggestions from its
parent when either lowercase filename contains the other; otherwise it returns
only `File not found`. The behavior is covered by a focused suggestion test.
([crates/hya-tool/src/read.rs:162-199](../../crates/hya-tool/src/read.rs#L162-L199),
[crates/hya-tool/tests/read_missing.rs:56-77](../../crates/hya-tool/tests/read_missing.rs#L56-L77))

## EDIT

### Schema and validation

The advertised EDIT schema requires `filePath`, `oldString`, and `newString`,
with optional `replaceAll`. Runtime deserialization also accepts the short
spellings `path`, `old`, `new`, and `replace_all`.
([crates/hya-tool/src/edit.rs:16-26](../../crates/hya-tool/src/edit.rs#L16-L26),
[crates/hya-tool/src/edit.rs:30-52](../../crates/hya-tool/src/edit.rs#L30-L52))

EDIT rejects identical old/new strings. An empty `oldString` is rejected for
an existing path, but for a missing path it creates parent directories and the
file from `newString`. This creation path is part of EDIT's implementation even
though WRITE is the explicit full-file/create tool.
([crates/hya-tool/src/edit.rs:54-98](../../crates/hya-tool/src/edit.rs#L54-L98))

Paths use the same lexical workdir resolution as READ. An outside path requires
`Action::ExternalDirectory` on its parent wildcard, and every call requires
`Action::Edit` on the resolved file before any content is changed.
([crates/hya-tool/src/edit.rs:62-67](../../crates/hya-tool/src/edit.rs#L62-L67),
[crates/hya-tool/src/edit.rs:163-177](../../crates/hya-tool/src/edit.rs#L163-L177))

### Matching and replacement

For an existing file, EDIT preserves its UTF-8 BOM and line-ending convention.
LF parameters are converted to CRLF before matching a CRLF file, and a BOM in
either the source or incoming replacement is retained exactly once.
([crates/hya-tool/src/edit.rs:99-113](../../crates/hya-tool/src/edit.rs#L99-L113),
[crates/hya-tool/src/edit_replace.rs:28-30](../../crates/hya-tool/src/edit_replace.rs#L28-L30),
[crates/hya-tool/tests/edit.rs:113-141](../../crates/hya-tool/tests/edit.rs#L113-L141),
[crates/hya-tool/tests/edit.rs:172-200](../../crates/hya-tool/tests/edit.rs#L172-L200))

Matching is deliberately broader than exact substring replacement. Candidates
are tried in this order: exact, line-trimmed, anchored block similarity,
whitespace-normalized, indentation-flexible, escape-normalized, trimmed
boundary, context-aware, and multi-occurrence. The first candidate family that
produces an acceptable match wins.
([crates/hya-tool/src/edit_replace/replacers.rs:1-12](../../crates/hya-tool/src/edit_replace/replacers.rs#L1-L12))

Without `replaceAll`, the selected candidate must occur exactly once. With
`replaceAll`, all occurrences of that selected candidate are replaced. EDIT
rejects missing matches, ambiguous matches, and fuzzy candidates whose matched
span is disproportionately larger than `oldString`.
([crates/hya-tool/src/edit_replace.rs:33-72](../../crates/hya-tool/src/edit_replace.rs#L33-L72),
[crates/hya-tool/src/edit_replace.rs:97-107](../../crates/hya-tool/src/edit_replace.rs#L97-L107))

Focused tests demonstrate line-trimmed, whitespace-normalized, anchored,
escape-normalized, trimmed-boundary, and context-aware matches. These are
contract behavior, not fallback behavior supplied by a provider.
([crates/hya-tool/tests/edit_fuzzy.rs:56-118](../../crates/hya-tool/tests/edit_fuzzy.rs#L56-L118),
[crates/hya-tool/tests/edit_fuzzy.rs:120-206](../../crates/hya-tool/tests/edit_fuzzy.rs#L120-L206),
[crates/hya-tool/tests/edit_fuzzy.rs:208-275](../../crates/hya-tool/tests/edit_fuzzy.rs#L208-L275))

### Post-edit processing and result

After writing, EDIT runs the configured formatter, restores the desired BOM if
the formatter changed it, touches the LSP plane, and collects diagnostics. A
formatter can therefore change the final file beyond the literal replacement;
a test explicitly installs a formatter that rewrites the whole file.
([crates/hya-tool/src/edit.rs:99-123](../../crates/hya-tool/src/edit.rs#L99-L123),
[crates/hya-tool/tests/edit.rs:143-170](../../crates/hya-tool/tests/edit.rs#L143-L170))

Success returns `created`, replacement count, relative `title`, human-readable
`output`, diagnostics, a unified diff, and addition/deletion metadata. The
shape and diff contents are covered by the EDIT result test. This result path
does not apply a local size cap to the diff or output.
([crates/hya-tool/src/edit.rs:126-156](../../crates/hya-tool/src/edit.rs#L126-L156),
[crates/hya-tool/tests/edit.rs:202-238](../../crates/hya-tool/tests/edit.rs#L202-L238))

## Local search: GLOB, FIND, and GREP

### Shared implementation

GLOB and GREP are in-process Rust implementations; they do not invoke
`ripgrep`. Their recursive walker uses `std::fs::read_dir`, descends into
directories, returns files only, and silently skips directories it cannot
read. Both cap returned rows at `SEARCH_LIMIT == 100`.
([crates/hya-tool/src/tool.rs:76-76](../../crates/hya-tool/src/tool.rs#L76-L76),
[crates/hya-tool/src/tool.rs:289-300](../../crates/hya-tool/src/tool.rs#L289-L300))

Path/include matching uses the same custom matcher as permission patterns. Its
only metacharacter is `*`; `?`, character classes, and path-aware `**`
semantics are not implemented. A pattern is tested against both the relative
path and basename.
([crates/hya-tool/src/permission.rs:335-362](../../crates/hya-tool/src/permission.rs#L335-L362),
[crates/hya-tool/src/tool.rs:312-323](../../crates/hya-tool/src/tool.rs#L312-L323))

Both tools resolve a relative `path` against the workdir, require an
action-specific permission on the pattern, and separately require
`ExternalDirectory` permission when the search root is outside the workdir.
([crates/hya-tool/src/tool.rs:325-347](../../crates/hya-tool/src/tool.rs#L325-L347),
[crates/hya-tool/src/tool.rs:371-390](../../crates/hya-tool/src/tool.rs#L371-L390),
[crates/hya-tool/src/tool.rs:469-489](../../crates/hya-tool/src/tool.rs#L469-L489))

### GLOB

GLOB requires `pattern` and optionally accepts a directory `path`, defaulting
to the workdir. Passing an existing file as `path` is an input error. It walks
recursively, matches files, sorts full paths lexically, and returns at most 100.
([crates/hya-tool/src/tool.rs:349-370](../../crates/hya-tool/src/tool.rs#L349-L370),
[crates/hya-tool/src/tool.rs:371-408](../../crates/hya-tool/src/tool.rs#L371-L408))

The result contains a relative `title`, count/truncation metadata,
human-readable absolute-path `output`, workdir-relative legacy `paths`, and the
pre-truncation `total`. No matches returns `No files found`.
([crates/hya-tool/src/tool.rs:409-441](../../crates/hya-tool/src/tool.rs#L409-L441),
[crates/hya-tool/tests/glob_grep.rs:83-129](../../crates/hya-tool/tests/glob_grep.rs#L83-L129))

`metadata.truncated` is computed as `total >= 100`, so it is true when there are
exactly 100 matches as well as when additional matches exist. It means the cap
was reached, not necessarily that a 101st result was found.
([crates/hya-tool/src/tool.rs:405-408](../../crates/hya-tool/src/tool.rs#L405-L408))

### FIND

FIND is a separate compatibility-oriented path matcher. It requires `pattern`,
optionally accepts `path`, uses the same recursive walker and `*` matcher, and
returns sorted `{path, size}` records. Unlike GLOB, this implementation has no
100-result cap and does not perform the shared external-directory check. A
supplied relative `path` is converted directly to `PathBuf`; it is not resolved
against `ToolCtx.workdir` as GLOB/GREP paths are.
([crates/hya-tool/src/tool.rs:640-690](../../crates/hya-tool/src/tool.rs#L640-L690))

### GREP

GREP requires a non-empty Rust regular expression `pattern`, optionally accepts
`path`, and optionally filters files with the custom `*`-only `include`
matcher. Invalid regex syntax is an input error.
([crates/hya-tool/src/tool.rs:445-475](../../crates/hya-tool/src/tool.rs#L445-L475))

If `path` identifies a file, GREP intentionally searches that file's **parent
directory**, not only the named file. The Compat behavior is explicit in code
and covered by a test where passing `src/main.rs` also returns a match from
`src/lib.rs`.
([crates/hya-tool/src/tool.rs:476-500](../../crates/hya-tool/src/tool.rs#L476-L500),
[crates/hya-tool/tests/glob_grep.rs:164-187](../../crates/hya-tool/tests/glob_grep.rs#L164-L187))

Files are sorted lexically, decoded with `tokio::fs::read_to_string`, and
searched line by line. Files that cannot be decoded/read are silently skipped.
Collection stops immediately at 100 matches, so GREP does not calculate the
actual total beyond the cap.
([crates/hya-tool/src/tool.rs:496-523](../../crates/hya-tool/src/tool.rs#L496-L523))

The result contains the regex as `title`, match-count/truncation metadata,
grouped human-readable output, structured `{file, line, text}` matches, and a
`total` equal to the number returned. No matches returns `No files found`.
As with GLOB, `truncated` becomes true at exactly 100 rows, which signals only
that collection reached the cap.
([crates/hya-tool/src/tool.rs:524-579](../../crates/hya-tool/src/tool.rs#L524-L579),
[crates/hya-tool/tests/glob_grep.rs:131-162](../../crates/hya-tool/tests/glob_grep.rs#L131-L162))

## Permissions and execution

The registry attaches invocation-level permission metadata to every canonical
name. READ, LS, GLOB, FIND, GREP, LSP, SKILL, LIST_AGENTS, ROSTER, and CHANNELS
are `ReadOnly`; TASK is `Task`; SHELL and BASH are `Command`; other builtins are
general `Tool` calls. Read-only/task invocations default to allow, general tool
invocations default to ask, command invocations extract the command string,
and MCP invocations use an MCP subject.
([crates/hya-tool/src/tool.rs:127-141](../../crates/hya-tool/src/tool.rs#L127-L141),
[crates/hya-tool/src/tool.rs:265-272](../../crates/hya-tool/src/tool.rs#L265-L272),
[crates/hya-tool/tests/tool.rs:148-188](../../crates/hya-tool/tests/tool.rs#L148-L188))

At normal app startup, the action-level snapshot explicitly allows READ, GLOB,
and GREP. Tools still make their own typed action/resource assertions. An
invocation grant satisfies later action checks except `ExternalDirectory`,
which always remains independently enforceable unless the permission model is
Danger.
([crates/hya-app/src/runtime.rs:475-481](../../crates/hya-app/src/runtime.rs#L475-L481),
[crates/hya-tool/src/permission.rs:496-532](../../crates/hya-tool/src/permission.rs#L496-L532),
[crates/hya-tool/src/permission.rs:534-570](../../crates/hya-tool/src/permission.rs#L534-L570))

The engine processes each model tool call by running plugin before-hooks,
resolving canonical names or aliases, authorizing the invocation, constructing
`ToolCtx`, executing the tool, and running after-hooks. Permission errors cannot
be rewritten by an after-hook. Success becomes `Event::ToolResult`; failure
becomes `Event::ToolError` with a structured error value and display message.
([crates/hya-core/src/engine.rs:37-50](../../crates/hya-core/src/engine.rs#L37-L50),
[crates/hya-core/src/engine/turn.rs:199-265](../../crates/hya-core/src/engine/turn.rs#L199-L265),
[crates/hya-core/src/engine/turn.rs:266-317](../../crates/hya-core/src/engine/turn.rs#L266-L317))

Tool errors are categorized as `input`, `permission`, `io`, `json`,
`cancelled`, or `unknown` and serialized as
`{"error":{"type":...,"message":...}}` in error events.
([crates/hya-tool/src/tool.rs:37-51](../../crates/hya-tool/src/tool.rs#L37-L51),
[crates/hya-core/src/engine/tool_error.rs:4-25](../../crates/hya-core/src/engine/tool_error.rs#L4-L25))

## Runtime planes and extensions

All builtin schemas are registered before runtime capabilities are considered.
`ToolCtx` carries permission, interaction, spawner, mailbox, todo, skills,
agent catalog, web search, LSP, formatter, workdir, session, and cancellation
planes/resources.
([crates/hya-tool/src/tool.rs:59-74](../../crates/hya-tool/src/tool.rs#L59-L74),
[crates/hya-app/src/runtime.rs:462-475](../../crates/hya-app/src/runtime.rs#L462-L475))

A bare `SessionEngine` starts with a disconnected mailbox and default
interaction, spawner, todo, skill, agent, websearch, formatter, and LSP planes.
The application replaces the interaction, spawner, mailbox, agent catalog, and
formatter planes and starts the mailbox service. Consequently, registry
presence alone does not prove that a plane-backed tool can return useful data;
for example, mailbox operations report that they are available only inside a
running team, and LSP reports when no server supports a file type.
([crates/hya-core/src/engine.rs:68-127](../../crates/hya-core/src/engine.rs#L68-L127),
[crates/hya-app/src/runtime.rs:489-522](../../crates/hya-app/src/runtime.rs#L489-L522),
[crates/hya-app/src/runtime.rs:539-542](../../crates/hya-app/src/runtime.rs#L539-L542),
[crates/hya-tool/src/mailbox.rs:190-194](../../crates/hya-tool/src/mailbox.rs#L190-L194),
[crates/hya-tool/src/lsp.rs:79-87](../../crates/hya-tool/src/lsp.rs#L79-L87))

### MCP tools

At startup, hya connects enabled MCP servers and adapts tools returned by
`tools/list`. Disabled or failed servers contribute no tools. Only MCP tools
whose input schema has `type: "object"` are accepted.
([crates/hya-mcp/src/manager.rs:49-89](../../crates/hya-mcp/src/manager.rs#L49-L89),
[crates/hya-mcp/src/manager.rs:105-133](../../crates/hya-mcp/src/manager.rs#L105-L133),
[crates/hya-mcp/src/bridge.rs:21-43](../../crates/hya-mcp/src/bridge.rs#L21-L43))

The model-facing name is `mcp__{server}__{tool}`, while execution sends the
remote tool's original name in `tools/call`. MCP adapters assert `Action::Mcp`
and are registered with `ToolPermission::Mcp`. Text and supported image/PDF
content is normalized into hya output and attachments.
([crates/hya-mcp/src/bridge.rs:31-80](../../crates/hya-mcp/src/bridge.rs#L31-L80),
[crates/hya-mcp/src/bridge.rs:83-103](../../crates/hya-mcp/src/bridge.rs#L83-L103),
[crates/hya-app/src/runtime.rs:462-468](../../crates/hya-app/src/runtime.rs#L462-L468))

### Plugin tools

Connected plugins contribute declared tools whose input schema has
`type: "object"`. Plugin tool names are preserved as declared rather than
namespaced, execution requires a session, and calls are forwarded to the
owning plugin. They are registered as general `ToolPermission::Tool` tools.
([crates/hya-plugin/src/plugin_tool.rs:17-32](../../crates/hya-plugin/src/plugin_tool.rs#L17-L32),
[crates/hya-plugin/src/plugin_tool.rs:35-58](../../crates/hya-plugin/src/plugin_tool.rs#L35-L58),
[crates/hya-plugin/src/host.rs:240-250](../../crates/hya-plugin/src/host.rs#L240-L250),
[crates/hya-app/src/runtime.rs:469-474](../../crates/hya-app/src/runtime.rs#L469-L474))

Registry names are unique across builtins, MCP tools, and plugin tools. A
collision is rejected; runtime startup logs and skips that extension tool.
MCP namespacing reduces MCP collisions, while unnamespaced plugin declarations
can collide directly with a builtin or another plugin.
([crates/hya-tool/src/tool.rs:186-200](../../crates/hya-tool/src/tool.rs#L186-L200),
[crates/hya-app/src/runtime.rs:463-473](../../crates/hya-app/src/runtime.rs#L463-L473))

## Verified boundaries

The strongest executable contracts for the focused tools are:

- READ file/directory/media/path/permission behavior:
  [crates/hya-tool/tests/read.rs:60-267](../../crates/hya-tool/tests/read.rs#L60-L267)
- READ text limits:
  [crates/hya-tool/tests/read_limits.rs:56-117](../../crates/hya-tool/tests/read_limits.rs#L56-L117)
- READ missing-path suggestions:
  [crates/hya-tool/tests/read_missing.rs:56-77](../../crates/hya-tool/tests/read_missing.rs#L56-L77)
- EDIT permissions, BOM/line endings, formatter, and result metadata:
  [crates/hya-tool/tests/edit.rs:80-238](../../crates/hya-tool/tests/edit.rs#L80-L238)
- EDIT fuzzy matching:
  [crates/hya-tool/tests/edit_fuzzy.rs:56-275](../../crates/hya-tool/tests/edit_fuzzy.rs#L56-L275)
- GLOB/GREP path, output, include, permission, and file-path widening:
  [crates/hya-tool/tests/glob_grep.rs:83-214](../../crates/hya-tool/tests/glob_grep.rs#L83-L214)
- Canonical names, hidden aliases, and registry permission metadata:
  [crates/hya-tool/tests/tool.rs:111-188](../../crates/hya-tool/tests/tool.rs#L111-L188)

The source-derived edge cases called out above are intentional descriptions of
current behavior, not broader compatibility guarantees. In particular, tests
do not currently pin GLOB/GREP behavior at exactly 100 rows, FIND's lack of an
external-directory check, or READ's partial `totalLines` value after the byte
cap. Those observations should be rechecked if the corresponding collectors or
permission paths change.
