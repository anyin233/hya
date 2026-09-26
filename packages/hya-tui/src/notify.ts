/**
 * Desktop notifications (docs/tui.md "Desktop notifications").
 *
 * The TUI sends an OSC 9 (`ESC ] 9 ; <body> BEL`) and an OSC 777
 * (`ESC ] 777 ; notify ; <title> ; <body> BEL`) sequence — the same event,
 * both formats, since terminals vary in which they honor — when a turn of
 * the open (root-viewed) session finishes (success or error), or a
 * permission/question ask arrives for it, while the terminal is
 * unfocused (state/focus, tracked through the terminal's focus reporting)
 * and the `notifications` preference (default on, `/notifications`) is on.
 *
 * An ask of a session outside the open tree (another TUI's or WebUI tab's
 * session, a headless run; delivered by the global stream) notifies too,
 * its detail naming the session (`<title> · in <n>. <session>`). A
 * subagent's turn end or ask does not notify: app/controller.ts routes by
 * `askFrameRoute`/`globalAskRoute` and the turn runner's own queue, and
 * notifies each ask id at most once.
 *
 * The WebUI host (`packages/hya-tui-web`) maps the same two OSC sequences to
 * a browser `Notification`, generically (ADR-0021): it does not know these
 * are hya's, just that the terminal emitted a notification request.
 */

/** Fixed notification title: the escape sequences travel over the wire (SSH, the WebUI), so there is no OS-level app identity to show instead. */
export const notificationTitle = "hya"

export type NotifyKind = "turnFinished" | "turnFailed" | "permission" | "question"

/** Whether to send a notification: the preference is on and the terminal is not focused. */
export function shouldNotify(options: { notifications: boolean; focused: boolean }): boolean {
  return options.notifications && !options.focused
}

/**
 * Strip control characters (they would break out of, or hide inside, the
 * OSC payload — a literal BEL or ESC would end the sequence early) and cap
 * the length so one runaway title/body cannot flood the terminal.
 */
export function sanitizeNotificationText(text: string, maxLength = 120): string {
  const cleaned = text.replace(/[\x00-\x1f\x7f]/g, " ").replace(/\s+/g, " ").trim()
  return cleaned.length > maxLength ? `${cleaned.slice(0, maxLength - 1)}…` : cleaned
}

/** The notification body for one kind of event; `detail` is the session title, tool/action name, or question title. */
export function notificationBody(kind: NotifyKind, detail: string): string {
  switch (kind) {
    case "turnFinished": return `Turn finished${detail ? ` · ${detail}` : ""}`
    case "turnFailed": return `Turn failed${detail ? `: ${detail}` : ""}`
    case "permission": return `Permission needed${detail ? `: ${detail}` : ""}`
    case "question": return `Question${detail ? `: ${detail}` : ""}`
  }
}

/** The OSC 9 + OSC 777 sequence for one notification, sanitized and truncated; write it straight to the terminal. */
export function notificationSequence(body: string, title = notificationTitle): string {
  const cleanTitle = sanitizeNotificationText(title, 60)
  const cleanBody = sanitizeNotificationText(body)
  return `\x1b]9;${cleanBody}\x07\x1b]777;notify;${cleanTitle};${cleanBody}\x07`
}
