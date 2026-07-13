# Findings

- Upstream source: `/chivier-disk/yanweiye/Projects/opencode-frontent-rs/opencode-origin`, version 1.17.9, commit `cf31029350820c6bfc0fbd0e052a79a067ee6116`.
- Upstream license: MIT, `Copyright (c) 2025 opencode`; full notice must accompany copied/substantial derived source.
- Upstream TUI is `packages/tui`, private package `@opencode-ai/tui`, using Bun, OpenTUI 0.3.4, SolidJS, and the generated OpenCode SDK.
- hya already implements broad OpenCode-compatible HTTP/SSE routes in `hya-server`; adaptation should target gaps proven by the imported TUI.
- Current Rust `hya` directly enters `hya_tui::Tui`; `hya-ts` needs a process/connection boundary rather than an in-process Rust client.
- Two independent planners agreed on the core boundary: direct SDK/HTTP/SSE use against hya, no OpenCode backend/worker/provider/updater/Console/remote-workspace runtime, and the Rust `hya` command retained unchanged.
- `hya_sdk::ServerHandle::spawn_hya_backend` already starts `hya-backend serve --bind 127.0.0.1:0`, parses readiness, puts the child in its own process group, and cleans up the group and listening port on drop. Reusing it is smaller and safer than a second Bun process supervisor.
- `hya-server` broadcasts permission lifecycle events, but `QuestionRequests` only stores/replies to pending questions. The Compat SSE routes do not currently receive `question.asked`, `question.replied`, or `question.rejected`; this is a demonstrated root-level gap for an SDK-driven TUI.
- The upstream TUI's direct network seam is `SDKProvider`: `@opencode-ai/sdk/v2` plus `/global/event` SSE. The OpenCode worker/RPC transport is a CLI/backend optimization and is not needed for hya.
- Reachable upstream package imports outside `packages/tui` are limited to SDK v2, TUI plugin contracts, small core utilities (`Global`, flags, version, flock/glob/which), and UI audio assets. Core utilities and assets should become hya-local; static built-in plugin support can remain without the external plugin loader/manager.
- Upstream visible branding includes terminal titles, logo, status command naming, docs/update/provider URLs, tips, default theme/sound names, state/config paths, and error links. Protocol/package identifiers and legal provenance need explicit audit allowlists rather than blind replacement.
- The user approved a Bun-dependent first distribution. A self-contained Bun/OpenTUI executable is deferred.
- Root workspace version is 0.32.4 and root changelog contains only 0.32.4, so the expected feature version is 0.33.0 with the existing changelog archived when implementation begins.
- The pinned SDK's retained legacy surface declares `/permission` items and `permission.asked` as `permission/patterns/metadata/always`, `question.replied` with `answers`, and every `/global/event` envelope with `directory`. Current hya payloads omit or mismatch those fields even though the SDK does no runtime validation.
- A real backend can expose permissions without production changes by omitting `--yolo` and calling `session.shell`. A deterministic local OpenAI-compatible SSE responder can invoke the existing `question` tool; no public question-trigger endpoint exists or is needed.
- Production installation of the pinned SDK also installs its unused `./server` and `./v2/server` exports, which spawn `opencode`. Prepared runtimes must remove those two exports and their server/process files after the locked install while retaining the SDK v2 client.
- Branding audits must inspect complete reachable source, including JSX text nodes, rather than only quoted literals. The missed status instruction was JSX text and the dead Console helper was an unimported source file that still shipped because installation copies the complete retained `src/` tree.
