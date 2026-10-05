import type { RenderNode } from "@hya/tui-sdk";
import type { InspectorState } from "../shared/contracts";

/** Presentation only: it cannot inspect the frontend machine's filesystem. */
export function renderInspector(state: InspectorState): RenderNode {
  switch (state.kind) {
    case "disconnected":
      return { kind: "column", children: [
        { kind: "text", text: "Disk inspector is not connected.", style: { color: "warning" } },
        { kind: "text", text: "Waiting for the frontend bundle API bridge.", style: { color: "muted" } },
      ] };
    case "loading":
      return { kind: "text", text: "Connecting to disk inspector…", style: { color: "muted" } };
    case "failed":
      return { kind: "text", text: `Disk inspector: ${state.message}`, style: { color: "error" } };
    case "ready":
      return { kind: "column", children: [
        { kind: "text", text: `Backend: ${state.info.machine.hostname}`, style: { color: "fg", bold: true } },
        { kind: "text", text: state.info.machine.platform, style: { color: "muted" } },
        { kind: "text", text: "Disk scanning is not implemented yet.", style: { color: "warning" } },
      ] };
  }
}
