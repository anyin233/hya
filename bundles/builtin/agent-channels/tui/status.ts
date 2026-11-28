/**
 * Unread mail status item for `hya/agent-channels`.
 * Derives unread count from folded `Projection::extensions["hya/agent-channels"]`
 * (`inboxes` vs `cursors`) and refreshes from `GET /inbox` via `api.bundleApi`
 * when the active session updates.
 */
import type { ExtensionContext, JsonValue, TuiExtensionApi } from "@hya/tui-sdk";

interface ChannelsProjection {
  readonly inboxes?: Readonly<Record<string, readonly number[]>>;
  readonly cursors?: Readonly<Record<string, number>>;
}

interface InboxApiReply {
  readonly unread?: readonly { readonly from?: string; readonly channel?: string; readonly body?: string }[];
  readonly total?: number;
  readonly cursor?: number;
}

export function unreadFromProjection(raw: JsonValue | undefined): number {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return 0;
  const state = raw as unknown as ChannelsProjection;
  if (!state.inboxes || typeof state.inboxes !== "object") return 0;
  const cursors = state.cursors && typeof state.cursors === "object" ? state.cursors : {};
  let unread = 0;
  for (const [handle, indices] of Object.entries(state.inboxes)) {
    if (!Array.isArray(indices)) continue;
    const cursor = typeof cursors[handle] === "number" ? cursors[handle]! : 0;
    for (const index of indices) {
      if (typeof index === "number" && index >= cursor) unread += 1;
    }
  }
  return unread;
}

export function registerChannelStatusItem(api: TuiExtensionApi): void {
  let apiUnread: number | undefined;
  let lastSessionId: string | undefined;

  api.onContext(async (context: ExtensionContext) => {
    const sessionId = context.session?.id;
    if (sessionId !== lastSessionId) {
      lastSessionId = sessionId;
      apiUnread = undefined;
    }
    if (!sessionId) return true;
    // Prefer synchronous projection when present; refresh via bundle.api /inbox when available.
    const projected = unreadFromProjection(api.extensionState.get());
    if (projected > 0) {
      apiUnread = projected;
      return true;
    }
    try {
      const inbox = await api.bundleApi.get<InboxApiReply>("/inbox", { sessionId });
      if (inbox && Array.isArray(inbox.unread)) {
        apiUnread = inbox.unread.length;
      }
    } catch {
      // Session may not be registered in a channel unit; keep projected count.
    }
    return true;
  });

  api.registerStatusItem({
    id: "unread-mail",
    label: "Mail",
    priority: 3,
    render: () => {
      const projected = unreadFromProjection(api.extensionState.get());
      const count = projected > 0 ? projected : (apiUnread ?? 0);
      return count > 0 ? `${count} unread` : null;
    },
  });
}
