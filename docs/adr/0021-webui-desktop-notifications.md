# Map OSC 9 / OSC 777 to a browser Notification in the WebUI host

The TUI (`packages/hya-tui`) wants to tell the user something happened —
a turn finished, or it needs a permission/question answer — while they are
not looking at the terminal. A local terminal already has a story for this:
OSC 9 (`ESC ] 9 ; <message> BEL`) and OSC 777
(`ESC ] 777 ; notify ; <title> ; <body> BEL`) are terminal escape sequences a
program sends to ask the terminal for a desktop notification; many terminal
emulators (iTerm2, rxvt, kitty via its own OSC, Windows Terminal) already
show one. The WebUI (ADR-0018, ADR-0019, ADR-0020) puts xterm.js in that
terminal's place, so it needs the same story, or the WebUI loses a feature
the terminal build already has.

## Decision

The WebUI's browser page (`packages/hya-tui-web/web/client.ts`) registers
xterm.js OSC handlers for `9` and `777` and maps either to a browser
`Notification`, only while the tab is not the one in front of the user
(`document.hidden` or the window lacks focus). This is generic: the page has
no notion of hya, a turn, or a session. It knows only that the program on the
PTY asked for a notification, the same way it knows nothing about `bash` or
`edit` when it renders their tool cards — those are TUI-drawn text, not host
behavior. `packages/hya-tui-web`'s job stays "run one fixed command and
render its terminal" (ADR-0018); this decision adds one more terminal
behavior to render faithfully, like OSC 52 clipboard writes already do
(docs/tui.md "Copy").

Payload handling:

- **OSC 9** (`packages/hya-tui-web/src/notify.ts` `osc9Notification`): the
  whole payload is the notification body; there is no title.
- **OSC 777** (`osc777Notification`): `<subcommand>;<title>;<body>`. Only the
  `notify` subcommand is a notification (rxvt defines other `777`
  subcommands the page ignores); a malformed payload (no subcommand, or a
  `notify` with no `;` at all) is dropped.
- The browser `Notification` permission prompt only opens on a user gesture:
  the page asks for it once, on the first pointer or key event on the
  terminal. A denial, or a browser with no `Notification` API, is silent —
  no retry, no error banner. Skipping this handshake (asking during page
  load) is disallowed by browsers and would silently fail anyway.
- The TUI sends both OSC 9 and OSC 777 for the same event (for terminals
  that honor only one), so the page could show two notifications for one
  event. The page dedupes this generically: `createNotificationDeduper`
  drops a repeat with the same body within a short window (250ms default) —
  body is the one field both sequences always carry, so this needs no
  notion of "the same event", only "the same text, again, right away". It
  also passes `tag` (`notificationTag`, derived from title+body) to `new
  Notification`, so the browser's own notification center coalesces a
  duplicate the window missed (a second tab, a repeat past 250ms) instead
  of stacking it. Neither mechanism parses what the text means; both key on
  opaque strings the page already has.

The TUI side (`packages/hya-tui/src/notify.ts`, `app/controller.ts`) decides
*when* to send: a turn of the open session finishing (success or error, not
a user-cancelled one) or a permission/question ask for it arriving, while
its own terminal-focus tracking (opentui's `CliRenderer` "focus"/"blur"
events, driven by the terminal's CSI `?1004` focus reporting) says the
terminal is not focused, and the `notifications` preference (`/notifications
[on|off]`, default on) is on. That logic is shared by a local terminal and
the WebUI: it lives once, in the TUI, not duplicated in the host.

## Considered options

- **A WebSocket "notify" frame alongside PTY bytes.** Rejected: it would
  need a new `hya.v1` wire message and hya-specific meaning in the host
  (ADR-0018's "the host runs a fixed command" and knows nothing about what
  the command means). OSC sequences are already bytes on the same PTY
  stream; nothing new to carry them.
- **Only OSC 777 (drop OSC 9).** Rejected for the TUI side: OSC 9 is the one
  simple terminals (and this host) are more likely to already support, so
  the TUI keeps sending both; the host maps both because it does not know
  which the TUI "means" more.
- **Correlate OSC 9 and OSC 777 by meaning (e.g. "the TUI always sends 777
  right after 9 for one event").** Rejected: that is hya-specific reasoning
  smuggled into a generic page. The adopted dedupe instead keys on the
  opaque body text and a short time window — true of "a program repeated
  itself", not "hya did a thing" — so it works the same for any program on
  the PTY, hya or not.
- **No dedupe; accept the double notification.** Rejected once raised as a
  visible bug: a user-facing OS notification duplicated for one event is a
  worse experience than the small, generic windowed check costs.
- **Request `Notification` permission at page load.** Rejected: browsers
  require a user gesture for the permission prompt to appear at all; asking
  earlier just fails silently and never asks again on some browsers.

## Consequences

- A local terminal that understands OSC 9/777 and the WebUI now behave the
  same way for hya's desktop notifications, with the decision logic in one
  place (the TUI).
- The WebUI shows exactly one browser notification per event even though
  the TUI sends two escape sequences for it, without the host knowing what
  an "event" is — the 250ms window and the `tag` are both generic. A
  program on the PTY that legitimately sends two distinct notifications
  with identical text within 250ms would also be deduped to one; accepted,
  since that is indistinguishable from a repeat without hya-specific
  knowledge, and matches what a real desktop notification center would
  likely coalesce anyway (`tag`).
- A future generic terminal-escape feature (e.g. OSC 8 hyperlinks, already
  in `TerminalCapabilities`) follows the same shape: the host maps it
  generically, without asking what program is on the other end of the PTY.
- Playwright specs (`packages/hya-tui-web/e2e/hya-tui-notifications.spec.ts`)
  cover the TUI's decision (OSC 9 payload, focus-gated, the preference) and
  the host's mapping (a stubbed `window.Notification`, gated on
  `document.hidden`/focus, asserted at exactly one call per event)
  separately, matching this split of responsibility.
