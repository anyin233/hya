/** Render-tree and text helpers shared by the panels (the TUI's former `state/format.ts` helpers). */
import type { NodeAction, RenderNode } from "@hya/tui-sdk";

/** Cut from the right with an ellipsis: `Long na…`. */
export function truncate(text: string, width: number): string {
  if (text.length <= width) return text;
  return width <= 1 ? "…".slice(0, width) : `${text.slice(0, width - 1)}…`;
}

/** Cut from the left, keeping the end (paths): `…/work`. */
export function truncateStart(text: string, width: number): string {
  return text.length <= width ? text : `…${text.slice(text.length - width + 1)}`;
}

/** `950`, `12.3k`, `456k`, `1.2M`. */
export function formatTokens(value: number): string {
  if (value < 1000) return String(value);
  if (value < 100_000) return `${(Math.floor(value / 100) / 10).toFixed(1).replace(/\.0$/, "")}k`;
  if (value < 1_000_000) return `${Math.floor(value / 1000)}k`;
  return `${(Math.floor(value / 100_000) / 10).toFixed(1).replace(/\.0$/, "")}M`;
}

/** One text node; `color` is a theme token (`fg`, `accent`, `muted`, `warning`, `error`, `success`, `border`). */
export function text(value: string, color: string, action?: NodeAction): RenderNode {
  return { kind: "text", text: value, style: { color }, ...(action ? { action } : {}) };
}

/** The `id` of a clicked row (`{ id }` action data), or `undefined`. */
export function actionId(data: unknown): string | undefined {
  return typeof data === "object" && data !== null && "id" in data && typeof data.id === "string" ? data.id : undefined;
}
