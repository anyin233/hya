/**
 * Generic desktop notifications (docs/tui-web.md "Desktop notifications";
 * ADR-0021): the page maps two OSC sequences any terminal program may send
 * to a browser `Notification`. The host does not know these come from hya —
 * OSC 9 (`ESC ] 9 ; <message> BEL`, iTerm2/rxvt's "simple" notify) and
 * OSC 777 (`ESC ] 777 ; notify ; <title> ; <body> BEL`, rxvt's structured
 * notify) are generic terminal escape sequences.
 *
 * xterm.js's OSC handler gives this module the payload after the OSC number
 * and its `;`, without the leading `ESC ]`/trailing terminator.
 */

export interface NotificationRequest {
  title: string
  body: string
}

/** OSC 9's payload is the message itself, with no title. */
export function osc9Notification(payload: string): NotificationRequest {
  return { title: "", body: payload }
}

/**
 * OSC 777's payload is `<subcommand>;<title>;<body>`; only `notify` is a
 * notification (rxvt also defines other `777` subcommands). `undefined` for
 * anything else, or a `notify` payload missing its `;` separators.
 */
export function osc777Notification(payload: string): NotificationRequest | undefined {
  const parts = payload.split(";")
  if (parts[0] !== "notify" || parts.length < 2) return undefined
  const [, title = "", ...rest] = parts
  return { title, body: rest.join(";") }
}

/** Only notify while the page is not the one the user is looking at. */
export function shouldShowNotification(page: { hidden: boolean; focused: boolean }): boolean {
  return page.hidden || !page.focused
}

/**
 * `Notification`'s own `tag`: two notifications with the same tag replace
 * each other in the OS notification center instead of stacking. Derived
 * from title+body only — generic, no notion of what the program meant.
 */
export function notificationTag(request: NotificationRequest): string {
  return `${request.title}\u0000${request.body}`
}

/**
 * A program may send more than one notify sequence for the same event (the
 * hya TUI sends both OSC 9 and OSC 777, for terminals that honor only one).
 * This dedupes near-simultaneous requests that carry the same body — the
 * one field both sequences always carry — within `windowMs` (default
 * 250ms), so the page shows one `Notification`, not one per sequence. It is
 * keyed only on the opaque body text; it has no notion of what the body
 * means.
 */
export function createNotificationDeduper(windowMs = 250) {
  const lastShown = new Map<string, number>()
  return {
    /** Whether a notification with this body should be shown now; records it as shown when it should. */
    shouldShow(body: string, now: number): boolean {
      for (const [key, at] of lastShown) if (now - at > windowMs) lastShown.delete(key)
      const last = lastShown.get(body)
      if (last !== undefined && now - last <= windowMs) return false
      lastShown.set(body, now)
      return true
    },
  }
}
