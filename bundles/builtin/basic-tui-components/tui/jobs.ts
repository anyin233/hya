/**
 * The Jobs pane: live activity across the active turn, busy sessions,
 * waiting sessions, and running child sessions. Clicking a row opens
 * that session (or its root session).
 */
import type { ExtensionContext, RenderNode, TuiExtensionApi } from "@hya/tui-sdk";
import { actionId, text, truncate } from "./text";

type SessionItem = NonNullable<ExtensionContext["sessions"]>["items"][number];

function childLabel(session: SessionItem, sessions: readonly SessionItem[]): string {
  const member = sessions
    .find((candidate) => candidate.id === session.parent)
    ?.members?.find((candidate) => candidate.child === session.id);
  return member?.handle || member?.description || member?.agent || session.agent;
}

function render(context: ExtensionContext, width: number): RenderNode {
  const rows: RenderNode[] = [];
  const active = context.session;
  const sessions = context.sessions?.items ?? [];

  if (active?.busy && active.id) {
    const label = active.title || active.id;
    rows.push(
      text(
        truncate(`● ${label} · turn running`, width),
        "activity",
        { name: "open", data: { id: active.id } },
      ),
    );
  }

  for (const session of sessions) {
    if (session.archived) continue;
    const isActiveRunning = Boolean(active?.busy && active.id === session.id);
    if (session.parent) {
      if (!session.busy && !session.waiting) continue;
      const state = session.waiting ? "waiting for input" : "running";
      const color = session.waiting ? "warning" : "activity";
      rows.push(
        text(
          truncate(`↳ ${childLabel(session, sessions)} · ${state}`, width),
          color,
          { name: "open", data: { id: session.id } },
        ),
      );
      continue;
    }
    if (session.waiting) {
      rows.push(
        text(
          truncate(`◌ ${session.title || session.id} · waiting for input`, width),
          "warning",
          { name: "open", data: { id: session.id } },
        ),
      );
      continue;
    }
    if (session.busy && !isActiveRunning) {
      rows.push(
        text(
          truncate(`● ${session.title || session.id} · busy`, width),
          "warning",
          { name: "open", data: { id: session.id } },
        ),
      );
    }
  }

  if (!rows.length) {
    return text("No active jobs. Busy sessions and waiting requests appear here live.", "muted");
  }

  return { kind: "column", children: rows };
}

export function registerJobs(api: TuiExtensionApi): void {
  api.registerPanel({
    id: "jobs",
    title: "Jobs",
    placement: "pane",
    replaces: "jobs",
    render: ({ context, width }) => render(context, width),
    onAction: (action) => {
      const id = actionId(action.data);
      if (!id) return { invalidate: false };
      return {
        invalidate: false,
        commands: [
          action.button === "right"
            ? { command: "session.menu", id }
            : { command: "session.open", id, token: "open" },
        ],
      };
    },
    onResult: (result) =>
      result.ok
        ? { invalidate: false }
        : { invalidate: false, notice: `Open failed: ${result.error ?? "unknown error"}` },
  });
}
