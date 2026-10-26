/**
 * The terminal title (docs/tui.md "Terminal title").
 *
 * The TUI names its terminal tab or window after the open session and its
 * Project, with a marker for what the session needs: `●` a turn is running,
 * `?` a permission/question ask waits for the user, `✓`/`✗` a turn ended
 * while the terminal was unfocused (cleared on focus). Most specific text
 * first: tab bars cut long titles off on the right.
 *
 * The title is written as OSC 0 (`ESC ] 0 ; <title> BEL`), only when it
 * changes. The title the terminal had before is saved with XTWINOPS
 * "push title" (`CSI 22 ; 0 t`) when the TUI starts and restored with
 * "pop title" (`CSI 23 ; 0 t`) when it exits; terminals without a title stack ignore both. The WebUI host
 * (`packages/hya-tui-web`) mirrors OSC 0/2 into the browser tab's
 * `document.title` generically (ADR-0030).
 */

import { basename } from "node:path"
import { sanitizeNotificationText } from "./notify"
import { promptQueue } from "./state/prompts"
import type { AppState } from "./state/store"

/** The fallback name when no session is open. */
export const appTitle = "hya"

/** Title length cap: long enough for a session title and Project, short enough for a tab. */
export const titleMaxLength = 80

/** What the open session needs, most urgent first; `finished`/`failed` only while unseen. */
export type TitleActivity = "waiting" | "working" | "finished" | "failed" | "idle"

export interface TitleInput {
  /** The open (root) session's title; empty or absent before one is open or titled. */
  session?: string
  /** The active Project's name, else the workspace directory's base name. */
  project?: string
  activity: TitleActivity
}

const markers: Record<TitleActivity, string> = {
  waiting: "?",
  working: "●",
  finished: "✓",
  failed: "✗",
  idle: "",
}

/** Per-part caps, so a long session title still leaves room for the Project. */
const sessionMaxLength = 48
const projectMaxLength = 28

/**
 * `sanitizeNotificationText` plus C1 controls: U+009C is ST and U+009B CSI
 * for terminals that honor 8-bit controls, so either could end the OSC early.
 */
function titlePart(text: string, maxLength: number): string {
  return sanitizeNotificationText(text.replace(/[\x80-\x9f]/g, " "), maxLength)
}

/** The title text: `[marker ]<session> · <project>`, or `hya · <project>` with no titled session. */
export function terminalTitle(input: TitleInput): string {
  const session = titlePart(input.session ?? "", sessionMaxLength)
  const project = titlePart(input.project ?? "", projectMaxLength)
  const name = session || appTitle
  // An unseen outcome is a glance-and-switch cue: the session name alone is enough.
  const showProject = project && project !== name && input.activity !== "finished" && input.activity !== "failed"
  const marker = markers[input.activity]
  const text = [marker, showProject ? `${name} · ${project}` : name].filter(Boolean).join(" ")
  return titlePart(text, titleMaxLength)
}

/** What the title reflects, read from the store (state/store.ts) by the caller. */
export interface TitleState {
  /** Asks of the open session tree (state/prompts.ts `promptQueue`). */
  waiting: boolean
  /** This TUI's turn is running (`running`, as the working indicator, state/activity.ts). */
  working: boolean
  /** A turn of the open session ended while the terminal was unfocused, not yet seen. */
  unseenOutcome?: "finished" | "failed"
}

export function titleActivity(state: TitleState): TitleActivity {
  if (state.waiting) return "waiting"
  if (state.working) return "working"
  return state.unseenOutcome ?? "idle"
}

/** The title for the store's current state; `directory` names the workspace when no Project is active. */
export function titleFromState(state: AppState, directory: string): string {
  const project = state.projects.find((row) => row.id === state.activeProjectId)?.name || basename(state.selected?.workdir || directory)
  return terminalTitle({
    session: state.selected?.title,
    project,
    activity: titleActivity({
      waiting: promptQueue(state.interactions, state).length > 0,
      working: state.running,
      unseenOutcome: state.unseenOutcome,
    }),
  })
}

/** OSC 0: set the icon name and window title. The text must already be sanitized (`terminalTitle`). */
export function titleSequence(title: string): string {
  return `\x1b]0;${title}\x07`
}

/** XTWINOPS: save the current icon name and window title on the terminal's title stack. */
export const pushTitleSequence = "\x1b[22;0t"
/** XTWINOPS: restore the icon name and window title saved by the last push. */
export const popTitleSequence = "\x1b[23;0t"

/** Writes the title only when it changes, between one push on `start` and one pop on `stop`. */
export class TitleWriter {
  private last: string | undefined
  private started = false

  constructor(private readonly write: (sequence: string) => void) {}

  /** Save the terminal's title; call once, when the TUI takes the terminal. */
  start(): void {
    if (this.started) return
    this.started = true
    this.write(pushTitleSequence)
  }

  /** Show `title` (from `terminalTitle`); a repeat writes nothing. */
  update(title: string): void {
    if (!this.started || title === this.last) return
    this.last = title
    this.write(titleSequence(title))
  }

  /** Restore the terminal's title; call once, after the renderer released the terminal. */
  stop(): void {
    if (!this.started) return
    this.started = false
    this.last = undefined
    this.write(popTitleSequence)
  }
}
