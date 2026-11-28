/**
 * Shared subagent state projection and normalization for `hya/multi-agent` TUI.
 * Reads folded `Projection::extensions["hya/multi-agent"]` plus optional `/members`
 * snapshots fetched via `api.bundleApi`.
 */
import type { ExtensionContext, JsonValue } from "@hya/tui-sdk";

export interface RawMemberProjection {
  readonly member?: string;
  readonly child?: string | null;
  readonly subagent_type?: string;
  readonly agent?: string;
  readonly description?: string;
  readonly depth?: number;
  readonly status?: string;
  readonly summary?: string;
  readonly handle?: string;
  readonly archived?: boolean;
  readonly archive_reason?: string;
}

export interface RawRosterEntry {
  readonly handle?: string;
  readonly session?: string;
  readonly agent_type?: string;
  readonly mode?: string;
  readonly status?: string;
  readonly current_task?: string | null;
}

export interface RawArchivedEntry {
  readonly handle?: string;
  readonly session?: string;
  readonly agent_type?: string;
  readonly reason?: string;
}

export interface MultiAgentSnapshot {
  readonly session?: string;
  readonly root?: string;
  readonly members?: readonly RawMemberProjection[];
  readonly roster?: Readonly<Record<string, RawRosterEntry>>;
  readonly archived?: Readonly<Record<string, RawArchivedEntry>>;
}

export interface SubagentRow {
  readonly sessionId: string;
  readonly label: string;
  readonly handle?: string;
  readonly agent: string;
  readonly status: string;
  readonly depth: number;
  readonly summary?: string;
  readonly archived: boolean;
  readonly archiveReason?: string;
}

export interface MultiAgentUiState {
  selectedSessionId?: string;
  showArchived: boolean;
  confirmArchiveSessionId?: string;
  follow: boolean;
  lastSessionId?: string;
  apiSnapshot?: MultiAgentSnapshot;
}

export function createUiState(): MultiAgentUiState {
  return {
    selectedSessionId: undefined,
    showArchived: false,
    confirmArchiveSessionId: undefined,
    follow: true,
    lastSessionId: undefined,
    apiSnapshot: undefined,
  };
}

export function truncate(text: string, width: number): string {
  if (width <= 0) return "";
  if (text.length <= width) return text;
  return width <= 1 ? "…".slice(0, width) : `${text.slice(0, width - 1)}…`;
}

function normalizeStatus(raw: string | undefined): string {
  if (!raw) return "idle";
  const lower = raw.toLowerCase().replace(/^member_status_/, "").replace(/^roster_status_/, "");
  if (lower === "spawning") return "starting";
  if (lower === "busy") return "running";
  return lower;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

export function decodeSnapshot(raw: JsonValue | undefined): MultiAgentSnapshot | undefined {
  const obj = asRecord(raw);
  if (!obj) return undefined;
  return obj as unknown as MultiAgentSnapshot;
}

/**
 * Derive visible subagent rows from `extension_state["hya/multi-agent"]`,
 * the latest `/members` API snapshot, and child session metadata in `context.sessions`.
 */
export function buildSubagentRows(
  context: ExtensionContext,
  extensionState: JsonValue | undefined,
  ui: MultiAgentUiState,
): SubagentRow[] {
  const snapshot = ui.apiSnapshot ?? decodeSnapshot(extensionState);
  const bySession = new Map<string, SubagentRow>();

  const rosterBySession = new Map<string, RawRosterEntry>();
  if (snapshot?.roster && typeof snapshot.roster === "object") {
    for (const [handleKey, entry] of Object.entries(snapshot.roster)) {
      if (entry?.session) {
        rosterBySession.set(entry.session, { ...entry, handle: entry.handle || handleKey });
      }
    }
  }

  const archivedBySession = new Map<string, RawArchivedEntry>();
  if (snapshot?.archived && typeof snapshot.archived === "object") {
    for (const [handleKey, entry] of Object.entries(snapshot.archived)) {
      if (entry?.session) {
        archivedBySession.set(entry.session, { ...entry, handle: entry.handle || handleKey });
      }
    }
  }

  for (const member of snapshot?.members ?? []) {
    const sessionId = typeof member.child === "string" && member.child ? member.child : undefined;
    if (!sessionId) continue;
    const roster = rosterBySession.get(sessionId);
    const archivedEntry = archivedBySession.get(sessionId);
    const archived = member.archived === true || archivedEntry !== undefined;
    const handle = member.handle || roster?.handle || archivedEntry?.handle;
    const agent = member.subagent_type || member.agent || roster?.agent_type || archivedEntry?.agent_type || "subagent";
    const label = handle || member.description || agent || sessionId;
    const status = archived ? "archived" : normalizeStatus(roster?.status || member.status);
    const summary = member.summary || member.description || roster?.current_task || undefined;
    bySession.set(sessionId, {
      sessionId,
      label,
      ...(handle ? { handle } : {}),
      agent,
      status,
      depth: typeof member.depth === "number" && member.depth >= 0 ? member.depth : 0,
      ...(summary ? { summary } : {}),
      archived,
      ...(member.archive_reason || archivedEntry?.reason ? { archiveReason: member.archive_reason || archivedEntry?.reason } : {}),
    });
  }

  for (const [sessionId, entry] of rosterBySession) {
    if (bySession.has(sessionId)) continue;
    const agent = entry.agent_type || "subagent";
    const handle = entry.handle;
    bySession.set(sessionId, {
      sessionId,
      label: handle || agent || sessionId,
      ...(handle ? { handle } : {}),
      agent,
      status: normalizeStatus(entry.status),
      depth: 0,
      ...(entry.current_task ? { summary: entry.current_task } : {}),
      archived: false,
    });
  }

  for (const [sessionId, entry] of archivedBySession) {
    if (bySession.has(sessionId)) continue;
    const agent = entry.agent_type || "subagent";
    const handle = entry.handle;
    bySession.set(sessionId, {
      sessionId,
      label: handle || agent || sessionId,
      ...(handle ? { handle } : {}),
      agent,
      status: "archived",
      depth: 0,
      archived: true,
      ...(entry.reason ? { archiveReason: entry.reason } : {}),
    });
  }

  // Fallback / enrichment from context.sessions child rows of the active session.
  const activeId = context.session?.id;
  if (activeId && context.sessions?.items) {
    const parentSession = context.sessions.items.find((s) => s.id === activeId);
    for (const member of parentSession?.members ?? []) {
      if (!member.child || bySession.has(member.child)) continue;
      const childSession = context.sessions.items.find((s) => s.id === member.child);
      const handle = member.handle;
      const agent = member.agent || childSession?.agent || "subagent";
      const archived = childSession?.archived === true;
      bySession.set(member.child, {
        sessionId: member.child,
        label: handle || member.description || agent,
        ...(handle ? { handle } : {}),
        agent,
        status: archived ? "archived" : childSession?.waiting ? "waiting" : childSession?.busy ? "running" : "idle",
        depth: 0,
        ...(member.description ? { summary: member.description } : {}),
        archived,
      });
    }
    for (const item of context.sessions.items) {
      if (item.parent !== activeId) continue;
      const existing = bySession.get(item.id);
      if (existing) {
        if (!existing.archived && (item.busy || item.waiting)) {
          bySession.set(item.id, {
            ...existing,
            status: item.waiting ? "waiting" : "running",
          });
        }
      } else {
        bySession.set(item.id, {
          sessionId: item.id,
          label: item.title || item.agent || item.id,
          agent: item.agent || "subagent",
          status: item.archived ? "archived" : item.waiting ? "waiting" : item.busy ? "running" : "idle",
          depth: 0,
          archived: item.archived === true,
        });
      }
    }
  }

  const all = [...bySession.values()];
  return ui.showArchived ? all : all.filter((row) => !row.archived);
}

export function resolveSelectedRow(rows: readonly SubagentRow[], ui: MultiAgentUiState): SubagentRow | undefined {
  if (!rows.length) return undefined;
  if (ui.selectedSessionId) {
    const match = rows.find((row) => row.sessionId === ui.selectedSessionId);
    if (match) return match;
  }
  return rows[0];
}
