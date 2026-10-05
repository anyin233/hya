/**
 * The Sessions pane: the active Project's sessions (and temporary ones) as a
 * tree in stable creation order. A top-level session is two lines (title,
 * agent) with a rule between groups; a subagent session is one indented line
 * with its member handle. A click opens the session (a subagent row opens its
 * root); a right click opens the session's actions.
 */
import type { ExtensionContext, RenderNode, TuiExtensionApi } from "@hya/tui-sdk";
import { actionId, text, truncate } from "./text";

type Session = NonNullable<ExtensionContext["sessions"]>["items"][number];

interface SessionRow {
  readonly session: Session;
  readonly depth: number;
  /** Roots are 1-based; descendants use x.y notation. */
  readonly number: string;
}

/** A running session's updates must not renumber every other session. */
function sessionTree(sessions: readonly Session[]): SessionRow[] {
  const ids = new Set(sessions.map((session) => session.id));
  const children = new Map<string, Session[]>();
  for (const session of sessions) {
    if (session.parent && ids.has(session.parent) && session.parent !== session.id) {
      children.set(session.parent, [...(children.get(session.parent) ?? []), session]);
    }
  }
  const stable = (a: Session, b: Session): number => (a.created ?? 0) - (b.created ?? 0) || a.id.localeCompare(b.id);
  for (const group of children.values()) group.sort(stable);
  const roots = sessions.filter((session) => !(session.parent && ids.has(session.parent))).sort(stable);
  const rows: SessionRow[] = [];
  const seen = new Set<string>();
  const visit = (session: Session, depth: number, number: string): void => {
    if (seen.has(session.id)) return;
    seen.add(session.id);
    rows.push({ session, depth, number });
    let childNumber = 0;
    for (const child of children.get(session.id) ?? []) {
      childNumber += 1;
      visit(child, depth + 1, `${number}.${childNumber}`);
    }
  };
  const nextRootNumber = (): string => String(rows.filter((row) => row.depth === 0).length + 1);
  for (const session of roots) visit(session, 0, nextRootNumber());
  for (const session of sessions) if (!seen.has(session.id)) visit(session, 0, nextRootNumber());
  return rows;
}

/** A subagent row names its member handle (the full minted name, e.g. hya-scout-skade), not just its agent class. */
function childLabel(session: Session, sessions: readonly Session[]): string {
  const member = sessions.find((candidate) => candidate.id === session.parent)?.members?.find((candidate) => candidate.child === session.id);
  return member?.handle || member?.description || member?.agent || session.agent;
}

function render(state: ExtensionContext["sessions"], width: number): RenderNode {
  if (!state?.ready) return text("Loading…", "fg");
  if (!state.items.length) return text("No sessions. Type a prompt or /new.", "fg");
  const rows: RenderNode[] = [];
  const row = (value: string, id: string): RenderNode => text(truncate(value, width), "fg", { name: "open", data: { id } });
  let announcedTemporary = false;
  for (const { session, depth, number } of sessionTree(state.items)) {
    const mark = session.id === state.selected ? "▸" : " ";
    const running = session.waiting ? " · ◌ waiting" : session.busy ? " · running" : "";
    if (depth > 0) {
      rows.push(row(`${mark}  ${"  ".repeat(depth - 1)}↳ ${number} ${childLabel(session, state.items)}${running}`, session.id));
      continue;
    }
    if (session.temporary && !announcedTemporary) {
      announcedTemporary = true;
      rows.push(text(truncate("— Temporary —", width), "fg"));
    }
    if (rows.length) rows.push(text(truncate("─".repeat(Math.max(1, width)), width), "border"));
    rows.push(row(`${mark} ${number}. ${session.title || session.id}`, session.id));
    rows.push(row(`   ${session.agent}${running}${session.archived ? " · archived" : ""}`, session.id));
  }
  return { kind: "column", children: rows };
}

export function registerSessions(api: TuiExtensionApi): void {
  api.registerPanel({
    id: "sessions",
    title: "Sessions",
    replaces: "sessions",
    render: ({ context, width }) => render(context.sessions, width),
    onAction: (action) => {
      const id = actionId(action.data);
      if (!id) return { invalidate: false };
      return { invalidate: false, commands: [action.button === "right" ? { command: "session.menu", id } : { command: "session.open", id, token: "open" }] };
    },
    onResult: (result) => result.ok ? { invalidate: false } : { invalidate: false, notice: `Open failed: ${result.error ?? "unknown error"}` },
  });
}
