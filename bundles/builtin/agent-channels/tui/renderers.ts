import type { RenderNode, ToolCallInput, ToolCardPresentation, TuiExtensionApi } from "@hya/tui-sdk";

function asObject(value: ToolCallInput["input"]): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function preview(text: string, max: number): string {
  const single = text.replace(/\s+/g, " ").trim();
  return single.length <= max ? single : `${single.slice(0, max - 1)}…`;
}

function render(call: ToolCallInput): ToolCardPresentation {
  const args = asObject(call.input);
  const output = typeof call.output === "string" ? call.output : asObject(call.output).output;
  const lines: RenderNode[] = [];
  let summary: string;
  let statusChip: string | undefined;
  switch (call.tool.replace(/^agent-channels__/, "")) {
    case "send": {
      const target = typeof args.channel === "string" && args.channel.trim() ? args.channel.trim() : "default";
      summary = `send → ${target}`;
      if (typeof args.body === "string" && args.body) lines.push({ kind: "text", text: preview(args.body, 140) });
      if (typeof output === "string" && !call.error) lines.push({ kind: "text", text: preview(output, 140), style: { color: "muted" } });
      break;
    }
    case "list_channel":
      summary = `channels · ${call.status === "completed" ? "snapshot" : call.status}`;
      if (!call.error) {
        for (const line of (typeof output === "string" ? output.trim() : call.summary).split(/\r?\n/).slice(0, 8)) {
          lines.push({ kind: "text", text: line.slice(0, 2048), style: { color: "muted" } });
        }
      }
      break;
    default: {
      const outcome = typeof args.outcome === "string" ? args.outcome.toLowerCase() : "done";
      summary = `report · ${outcome}`;
      statusChip = outcome;
      if (typeof args.result === "string" && args.result) lines.push({ kind: "text", text: preview(args.result, 200) });
    }
  }
  return { summary, ...(statusChip ? { statusChip } : {}), expandedByDefault: true, body: { kind: "column", children: lines } };
}

export function registerChannelRenderers(api: TuiExtensionApi): void {
  api.registerToolRenderer({
    id: "channel-tools",
    tools: ["send", "list_channel", "report", "agent-channels__send", "agent-channels__list_channel", "agent-channels__report"],
    priority: 10,
    render,
  });
}
