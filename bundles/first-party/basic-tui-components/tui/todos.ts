/** The Todos pane: the open session's todo list, one line per item, scrolling with the pane. */
import type { ExtensionContext, RenderNode, TuiExtensionApi } from "@hya/tui-sdk";
import { text, truncate } from "./text";

type Todo = NonNullable<ExtensionContext["todos"]>[number];

const glyphs: Record<Todo["status"], string> = { pending: "○", in_progress: "◐", blocked: "✗", completed: "✓" };

/** Pending and blocked muted, in progress accent, completed green. */
const glyphColor = (status: Todo["status"]): string => status === "in_progress" ? "accent" : status === "completed" ? "success" : "muted";

function render(todos: ExtensionContext["todos"], width: number): RenderNode {
  if (!todos?.length) return text("No todos yet", "muted");
  return {
    kind: "column",
    children: todos.map((todo): RenderNode => ({
      kind: "row",
      children: [
        text(glyphs[todo.status], glyphColor(todo.status)),
        text(` ${truncate(todo.content, Math.max(1, width - 2))}`, todo.status === "completed" ? "muted" : "fg"),
      ],
    })),
  };
}

export function registerTodos(api: TuiExtensionApi): void {
  api.registerPanel({ id: "todos", title: "Todos", replaces: "todos", render: ({ context, width }) => render(context.todos, width) });
}
