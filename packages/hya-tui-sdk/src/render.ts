/**
 * Declarative content only: no callbacks, terminal escapes, or OpenTUI objects.
 * The host validates every tree (kinds, fields, depth, size) and strips terminal
 * escapes; an invalid tree is shown as an error instead of the surface.
 */
import type { JsonValue } from "./protocol";

export interface TextStyle {
  /** `#rrggbb` or a theme token: `fg`, `accent`, `muted`, `error`, `warning`, `success`, `border`. */
  readonly color?: string;
  readonly background?: string;
  readonly bold?: boolean;
  readonly italic?: boolean;
  readonly underline?: boolean;
}

/** A click on the node sends `tui/action` with this name and data (needs `tui.action`). */
export interface NodeAction {
  readonly name: string;
  readonly data?: JsonValue;
}

export type RenderNode =
  | { readonly kind: "text"; readonly text: string; readonly style?: TextStyle; readonly action?: NodeAction }
  | { readonly kind: "row" | "column"; readonly children: readonly RenderNode[]; readonly gap?: number }
  | { readonly kind: "box"; readonly children: readonly RenderNode[]; readonly title?: string; readonly border?: boolean; readonly padding?: number }
  | { readonly kind: "table"; readonly columns: readonly string[]; readonly rows: readonly (readonly string[])[] }
  | { readonly kind: "progress"; readonly value: number; readonly total: number; readonly label?: string }
  /** Decorating renderers only: where the built-in (or lower-priority) rendering goes. Exactly one per tree. */
  | { readonly kind: "slot" };

/** Host limits for one tree; larger trees are rejected. */
export const RENDER_LIMITS = { maxDepth: 32, maxNodes: 2_000, maxStringBytes: 32 * 1024 } as const;
