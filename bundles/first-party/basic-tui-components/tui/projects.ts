/**
 * The left Projects pane: one row per Project with the highlight `▸`, a busy
 * marker `●` while a session of it runs a turn, the active one in the accent
 * color, and its session count; a rule between rows. A click switches; a right
 * click opens the Project's actions. While it holds the keyboard (Ctrl+P, or a
 * click on the pane), Up/Down move the highlight, Enter switches and returns
 * the keyboard, Esc returns it without switching.
 */
import type { ExtensionContext, HandlerResult, KeyEvent, RenderNode, TuiExtensionApi } from "@hya/tui-sdk";
import { actionId, text, truncate } from "./text";

type Projects = NonNullable<ExtensionContext["projects"]>;

const isEnter = (key: KeyEvent): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter");

export function registerProjects(api: TuiExtensionApi): void {
  /** The highlighted row; the active Project until the keys move it. */
  let highlighted: string | undefined;
  const current = (projects: Projects | undefined): string | undefined => highlighted ?? projects?.active;

  const render = (projects: Projects | undefined, width: number): RenderNode => {
    if (!projects?.items.length) return text("No projects yet", "muted");
    const rows: RenderNode[] = [];
    for (const project of projects.items) {
      if (rows.length) rows.push(text("─".repeat(Math.max(1, width)), "border"));
      const action = { name: "switch", data: { id: project.id } };
      rows.push({
        kind: "row",
        children: [
          text(project.id === current(projects) ? "▸ " : "  ", "accent", action),
          text(project.busy ? "● " : "  ", project.busy ? "warning" : "fg", action),
          text(truncate(project.name, Math.max(1, width - 8)), project.id === projects.active ? "accent" : "fg", action),
          text(` (${project.sessionCount ?? 0})`, "muted", action),
        ],
      });
    }
    return { kind: "column", children: rows };
  };

  const key = (key: KeyEvent, projects: Projects | undefined): HandlerResult => {
    if (key.name === "escape") return { invalidate: false, commands: [{ command: "ui.release" }] };
    const items = projects?.items ?? [];
    if (!items.length) return { invalidate: false };
    const at = Math.max(0, items.findIndex((project) => project.id === current(projects)));
    if (key.name === "up" || key.name === "down") {
      highlighted = items[(at + (key.name === "up" ? -1 : 1) + items.length) % items.length]!.id;
      return { invalidate: true };
    }
    if (isEnter(key)) return { invalidate: false, commands: [{ command: "ui.release" }, { command: "project.switch", id: items[at]!.id, token: "switch" }] };
    return { invalidate: false };
  };

  api.registerPanel({
    id: "projects",
    title: "Projects",
    replaces: "projects",
    render: ({ context, width }) => render(context.projects, width),
    onKey: (pressed, context) => key(pressed, context.projects),
    onAction: (action) => {
      const id = actionId(action.data);
      if (!id) return { invalidate: false };
      if (action.button === "right") return { invalidate: false, commands: [{ command: "project.menu", id }] };
      return { invalidate: false, commands: [{ command: "ui.release" }, { command: "project.switch", id, token: "switch" }] };
    },
    onResult: (result) => result.ok ? { invalidate: false } : { invalidate: false, notice: `Switch failed: ${result.error ?? "unknown error"}` },
  });
}
