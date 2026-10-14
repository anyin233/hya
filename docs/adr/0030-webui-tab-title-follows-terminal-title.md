# The WebUI tab title follows the terminal title

Status: Accepted, 2026-10-14.

The TUI names its terminal after the open session and marks what it needs
(`●` running, `?` waiting on an ask, `✓`/`✗` a turn ended while away;
docs/tui.md "Terminal title"). It does this with OSC 0, the escape sequence
programs use to set a terminal emulator's tab or window title. The WebUI
(ADR-0018, ADR-0019) puts xterm.js in that emulator's place. Without a
mapping, a browser tab running the TUI would always read `hya`, and the
WebUI would lose the "which tab needs me" cue that a local terminal has.

## Decision

The WebUI's browser page (`packages/hya-tui-web/web/client.ts`) subscribes to
xterm.js `onTitleChange`, which fires for OSC 0 and OSC 2, and sets
`document.title` to the reported title. An empty or blank title shows the
page's own initial title again (`src/title.ts` `pageTitle`).

This is generic, like the OSC 9/777 notification mapping (ADR-0021) and OSC
52 clipboard writes: the page does not know that hya set the title or what
its markers mean, only that the program on the PTY set a title. The host's
job stays "run one fixed command and render its terminal" (ADR-0018).

The TUI decides what the title says and when it changes. That logic lives
once, in `packages/hya-tui/src/title.ts`, and a local terminal and the WebUI
share it.

## Consequences

- Any program run through the host that sets a title gets it in the browser
  tab.
- The XTWINOPS title stack (`CSI 22 ; 0 t` / `CSI 23 ; 0 t`) the TUI uses to
  restore the terminal's previous title is not modeled by the page. When the
  TUI exits, the WebUI session ends with it, so there is nothing to restore.
- The title is always on, with no preference to turn it off. A user whose tmux
  or shell manages titles sees hya's title while the TUI runs and their own
  again after it exits.
