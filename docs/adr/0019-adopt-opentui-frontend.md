# Adopt the Bun/OpenTUI frontend as the interactive TUI

ADR-0005 and ADR-0010 removed the legacy TUIs and left hya with no interactive
frontend until a replacement was decided. A Bun/OpenTUI frontend
(`packages/hya-tui`) was built on a contributor fork over the `hya.v1`
HTTP/JSON+SSE contract. It covers sessions, the transcript, turns, pending
interactions, models, Workflows, saved provider keys, slash completion, and a
generic `/api` view.

## Decision

`packages/hya-tui` is hya's interactive TUI, and further TUI development
builds on it.

- It is a pure v1 client. The transcript is read from the server projection
  (`MessageInfo.parts`) after SSE notifications, and it keeps no competing
  durable state.
- It runs from source with Bun. It is not bundled into the `hya` binary or the
  release archive; packaging it is a separate decision.
  *Amended by [ADR-0020](0020-bundle-tui-and-webui-in-hya.md): the release
  archive now ships it as `lib/hya/tui`, and bare `hya` on a terminal starts it
  with the WebUI. It is still a separate Bun program, not code in the binary.*
- It is previewed and tested only through the browser rendering in
  `packages/hya-tui-web` (ADR-0018). That rendering is also the WebUI.

## Consequences

- There is one interactive frontend. A Rust or ratatui TUI, a second
  TypeScript terminal frontend, or backend-side rendering still needs its own
  ADR.
- Backend features the TUI needs land as ordinary `hya.v1` rpcs (for example
  `Auth.ListProviderAuth`), never as TUI-only routes.
