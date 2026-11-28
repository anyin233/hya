import type { RenderNode, ToolCallInput, ToolCardPresentation, TuiExtensionApi, ExtensionContext } from "@hya/tui-sdk";

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function text(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function taskCard(call: ToolCallInput, context: ExtensionContext): ToolCardPresentation {
  const input = record(call.input);
  const output = record(call.output);
  const metadata = record(output.metadata);
  const batch = Array.isArray(input.tasks) ? input.tasks : input.members;
  const specs = Array.isArray(batch) ? batch.map(record) : [];
  const outcomes = Array.isArray(metadata.members) ? metadata.members.map(record) : [];
  const agent = text((specs[0] ?? input).subagent_type) || text(metadata.subagent_type) || "hya-task";
  const description = text((specs[0] ?? input).description) ?? text(output.title) ?? "";
  const count = Math.max(specs.length, outcomes.length);
  const children: RenderNode[] = [];
  for (let index = 0; call.status !== "error" && index < (count || 1); index++) {
    const spec = count ? specs[index] ?? {} : input;
    const outcome = count ? outcomes[index] ?? {} : metadata;
    const child = text(outcome.sessionId) || text(outcome.session) || (!count ? text(output.child_session_id) : undefined);
    const handle = text(outcome.handle) || text(outcome.member);
    const label = handle || (count ? text(outcome.subagent_type) || text(spec.subagent_type) || "hya-task" : agent);
    const detail = count ? text(outcome.description) ?? text(spec.description) : description;
    const session = child ? context.sessions?.items.find((row) => row.id === child) : undefined;
    const reported = count ? text(outcome.status) : undefined;
    const status = reported === "completed" ? "done" : reported === "error" ? "failed"
      : reported ?? (session ? session.busy ? "running" : "idle" : call.status === "completed" ? "done" : call.status === "pending" ? "starting" : call.status);
    const badge = session?.waiting ? "waiting for approval or an answer" : status;
    const color = session?.waiting || status === "cancelled" ? "warning" : status === "running" ? "activity"
      : status === "done" ? "success" : status === "failed" ? "error" : "muted";
    const summary = count ? text(outcome.summary) : undefined;
    const action = child ? { name: "open-child", data: child } : undefined;
    children.push(
      { kind: "text", text: [label, detail].filter(Boolean).join(" · "), ...(action ? { action } : {}) },
      { kind: "text", text: summary ? `${badge}  ↳ ${summary}` : badge, style: { color } },
    );
    if (action) children.push({ kind: "text", text: `click to view · /open ${child}`, action });
  }
  return {
    summary: [agent, description, specs.length > 1 ? `${specs.length} members` : ""].filter(Boolean).join(" · "),
    expandedByDefault: true,
    body: { kind: "column", children },
  };
}

export function registerToolCards(api: TuiExtensionApi): void {
  api.registerToolRenderer({
    id: "task-card",
    tools: ["task", "multi-agent__task"],
    render: (call, { context }) => taskCard(call, context),
    onAction: ({ name, data }) => {
      if (name === "open-child" && typeof data === "string") {
        return { commands: [{ command: "session.open", id: data, exact: true }] };
      }
    },
  });
  api.registerToolRenderer({
    id: "member-tools",
    tools: ["list_agents", "search_agent", "archive", "wait", "multi-agent__list_agents", "multi-agent__search_agent", "multi-agent__archive", "multi-agent__wait"],
    render: (call) => {
      const output = text(record(call.output).output) ?? text(call.output) ?? JSON.stringify(call.output, null, 2);
      return {
        summary: call.summary,
        ...(output ? { body: { kind: "text", text: output.slice(0, 2048) } } : {}),
      };
    },
  });
}
