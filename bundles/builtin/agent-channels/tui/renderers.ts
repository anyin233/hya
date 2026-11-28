/**
 * Tool-call card renderers for `hya/agent-channels` (`send`, `list_channel`, `report`).
 */
import type { JsonValue, RenderNode, ToolCallInput, TuiExtensionApi } from "@hya/tui-sdk";

function asObject(value: JsonValue | undefined): Record<string, unknown> | undefined {
  if (typeof value === "object" && value !== null && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }
  return undefined;
}

function previewText(raw: string, max = 140): string {
  const single = raw.replace(/\s+/g, " ").trim();
  if (single.length <= max) return single;
  return `${single.slice(0, max - 1)}…`;
}

function statusGlyph(status: ToolCallInput["status"]): { glyph: string; color: string } {
  switch (status) {
    case "completed":
      return { glyph: "✓", color: "success" };
    case "error":
      return { glyph: "✗", color: "error" };
    case "running":
      return { glyph: "◐", color: "activity" };
    default:
      return { glyph: "○", color: "muted" };
  }
}

function renderSendCard(input: ToolCallInput): RenderNode {
  const args = asObject(input.input);
  const target = typeof args?.channel === "string" && args.channel.trim() ? args.channel.trim() : "default";
  const body = typeof args?.body === "string" ? args.body : "";
  const { glyph, color } = statusGlyph(input.status);
  const children: RenderNode[] = [
    {
      kind: "row",
      children: [
        { kind: "text", text: `${glyph} `, style: { color, bold: true } },
        { kind: "text", text: "send", style: { color: "heading", bold: true } },
        { kind: "text", text: ` → ${target}`, style: { color: "focus" } },
      ],
    },
  ];
  if (body) {
    children.push({ kind: "text", text: previewText(body), style: { color: "fg" } });
  }
  if (input.error) {
    children.push({ kind: "text", text: input.error, style: { color: "error" } });
  } else if (typeof input.output === "string" && input.output.trim()) {
    children.push({ kind: "text", text: previewText(input.output), style: { color: "muted" } });
  }
  return { kind: "column", children };
}

function renderListChannelCard(input: ToolCallInput): RenderNode {
  const { glyph, color } = statusGlyph(input.status);
  const children: RenderNode[] = [
    {
      kind: "row",
      children: [
        { kind: "text", text: `${glyph} `, style: { color, bold: true } },
        { kind: "text", text: "channels", style: { color: "heading", bold: true } },
        {
          kind: "text",
          text: input.status === "completed" ? " · snapshot" : ` · ${input.status}`,
          style: { color: "muted" },
        },
      ],
    },
  ];
  if (input.error) {
    children.push({ kind: "text", text: input.error, style: { color: "error" } });
  } else if (typeof input.output === "string" && input.output.trim()) {
    const lines = input.output.trim().split(/\r?\n/).slice(0, 8);
    for (const line of lines) {
      children.push({ kind: "text", text: line, style: { color: "muted" } });
    }
  } else if (input.summary) {
    children.push({ kind: "text", text: input.summary, style: { color: "muted" } });
  }
  return { kind: "column", children };
}

function renderReportCard(input: ToolCallInput): RenderNode {
  const args = asObject(input.input);
  const outcome = typeof args?.outcome === "string" ? args.outcome.toLowerCase() : "done";
  const result = typeof args?.result === "string" ? args.result : "";
  const failed = outcome === "failed" || input.status === "error";
  const badgeColor = failed ? "error" : input.status === "completed" ? "success" : "activity";
  const glyph = failed ? "✗" : input.status === "completed" ? "✓" : "◐";
  const children: RenderNode[] = [
    {
      kind: "row",
      children: [
        { kind: "text", text: `${glyph} `, style: { color: badgeColor, bold: true } },
        { kind: "text", text: "report", style: { color: "heading", bold: true } },
        { kind: "text", text: ` · ${outcome}`, style: { color: badgeColor, bold: true } },
      ],
    },
  ];
  if (result) {
    children.push({ kind: "text", text: previewText(result, 200), style: { color: "fg" } });
  }
  if (input.error) {
    children.push({ kind: "text", text: input.error, style: { color: "error" } });
  }
  return { kind: "column", children };
}

export function registerChannelRenderers(api: TuiExtensionApi): void {
  api.registerRenderer({
    id: "channel-tools",
    target: "tool_call",
    mode: "replace",
    priority: 10,
    render: (input) => {
      switch (input.tool) {
        case "send":
        case "agent-channels__send":
          return renderSendCard(input);
        case "list_channel":
        case "agent-channels__list_channel":
          return renderListChannelCard(input);
        case "report":
        case "agent-channels__report":
          return renderReportCard(input);
        default:
          return null;
      }
    },
  });
}
