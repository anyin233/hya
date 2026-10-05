/**
 * The full-screen Project view (`/project`, `/projects`): list every Project,
 * open/switch one, create one (name, then one root per step; the first root is
 * primary), edit an existing one's roots (add, remove, reorder), rename,
 * delete (the server's refusal shown verbatim while a live session blocks
 * it), and start a temporary session. Tab completes a root path from the
 * backend filesystem. The host runs the calls (host commands); this file holds
 * the view's state and keys.
 */
import type { CommandResult, ExtensionContext, HandlerResult, HostCommand, KeyEvent, RenderNode, TuiExtensionApi } from "@hya/tui-sdk";
import { actionId, text, truncate } from "./text";

type Project = NonNullable<ExtensionContext["projects"]>["items"][number];

interface Busy {
  readonly label: string;
  /** Host clock (`RenderContext.now`) when it started; 0 until the next render stamps it. */
  readonly startedAt: number;
}

interface Notice {
  readonly text: string;
  readonly tone: "info" | "ok" | "error";
}

/** The name-then-roots prompt for a new Project (`n`). */
interface CreateFlow {
  readonly step: "name" | "root";
  readonly name: string;
  readonly roots: readonly string[];
  readonly input: string;
}

/** The rename prompt (`r`). */
interface RenameFlow {
  readonly id: string;
  readonly input: string;
}

/** The roots editor (`e`). */
interface EditRootsFlow {
  readonly id: string;
  readonly roots: readonly string[];
  readonly selected: number;
  /** A text input is open to add a root. */
  readonly adding: boolean;
  readonly input: string;
}

interface View {
  readonly highlighted: string | undefined;
  readonly busy: Busy | undefined;
  readonly notice: Notice | undefined;
  /** Set while `d` asks to confirm a delete. */
  readonly confirm: string | undefined;
  readonly create: CreateFlow | undefined;
  readonly rename: RenameFlow | undefined;
  readonly editRoots: EditRootsFlow | undefined;
}

const closedView: View = { highlighted: undefined, busy: undefined, notice: undefined, confirm: undefined, create: undefined, rename: undefined, editRoots: undefined };

/** What a pending host command was for, so its result lands in the right place. */
type Pending =
  | { readonly kind: "switch" }
  | { readonly kind: "temporary" }
  | { readonly kind: "create" }
  | { readonly kind: "rename"; readonly name: string }
  | { readonly kind: "roots" }
  | { readonly kind: "delete" }
  | { readonly kind: "complete" };

const spinnerFrames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const printable = (key: KeyEvent): boolean => !key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " " && key.sequence !== "\x7f";
const isEnter = (key: KeyEvent): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter");

/** Keep the highlight on its row after the list changes (the first row when it is gone). */
function settle(view: View, projects: readonly Project[]): View {
  if (!projects.length) return view.highlighted === undefined ? view : { ...view, highlighted: undefined };
  return projects.some((project) => project.id === view.highlighted) ? view : { ...view, highlighted: projects[0]!.id };
}

function move(view: View, projects: readonly Project[], step: number): View {
  if (!projects.length) return view;
  const at = Math.max(0, projects.findIndex((project) => project.id === view.highlighted));
  return { ...view, highlighted: projects[(at + step + projects.length) % projects.length]!.id };
}

/** Editing a plain text field (create, rename, add-root inputs). */
function editText(input: string, key: KeyEvent): { text?: string; commit?: boolean; cancel?: boolean } {
  if (key.name === "escape") return { cancel: true };
  if (isEnter(key)) return { commit: true };
  if (key.name === "backspace") return { text: input.slice(0, -1) };
  if (printable(key)) return { text: input + key.sequence };
  return {};
}

/** The footer hint for the current screen or sub-flow. */
function hint(view: View): string {
  if (view.busy) return `${view.busy.label}… · Esc cancels`;
  if (view.create) return view.create.step === "name" ? "Type a name · Enter continues · Esc cancels" : "Type a root path · Enter adds it · Enter on empty finishes · Esc cancels";
  if (view.rename) return "Type a new name · Enter renames · Esc cancels";
  if (view.editRoots) {
    if (view.editRoots.adding) return "Type a root path · Enter adds it · Esc cancels";
    return "Up/Down select · Shift+Up/Down reorder (first = primary) · a add · d remove · Enter saves · Esc cancels";
  }
  if (view.confirm) return "Enter deletes · Esc cancels";
  return "Up/Down move · Enter opens/switches · n new · e edit roots · r rename · d delete · t temporary session · Esc close";
}

const noticeColor = (notice: Notice): string => notice.tone === "error" ? "error" : notice.tone === "ok" ? "accent" : "fg";

/** The first rows that keep row `index` visible in `height` rows. */
function windowStart(count: number, index: number, height: number): number {
  return count <= height ? 0 : Math.min(Math.max(0, index - height + 1), count - height);
}

function render(view: View, projects: Projects, width: number, height: number, now: number): RenderNode {
  const items = projects.items;
  const room = Math.max(20, width - 2);
  const rows: RenderNode[] = [text(`${items.length} project${items.length === 1 ? "" : "s"}`, "fg")];
  const visible = Math.max(3, height - 10);
  const start = windowStart(items.length, Math.max(0, items.findIndex((project) => project.id === view.highlighted)), visible);
  for (const project of items.slice(start, start + visible)) {
    const action = { name: "switch", data: { id: project.id } };
    const sessions = project.sessionCount ?? 0;
    rows.push({
      kind: "row",
      children: [
        text(project.id === view.highlighted ? "▸ " : "  ", "accent", action),
        text(project.busy ? "● " : "  ", project.busy ? "warning" : "fg", action),
        text(truncate(project.name, Math.max(1, Math.floor(room * 0.4))), project.id === projects.active ? "accent" : "fg", action),
        text(`  ${sessions} session${sessions === 1 ? "" : "s"} · ${truncate(project.roots[0] ?? "", Math.max(1, Math.floor(room * 0.4)))}`, "muted", action),
      ],
    });
  }
  if (!items.length) rows.push(text("  No projects yet · n creates one", "muted"));
  const prompt = (label: string, input: string, after: RenderNode[] = []): RenderNode => ({ kind: "row", children: [text(label, "muted"), text(input, "fg"), text("▏", "accent"), ...after] });
  if (view.create) {
    const flow = view.create;
    const label = flow.step === "name" ? "New project name " : `Root ${flow.roots.length + 1} (${flow.roots.length ? "Enter empty to finish" : "primary"}) `;
    rows.push(prompt(label, flow.input, flow.roots.length ? [text(`  · roots so far: ${flow.roots.join(", ")}`, "muted")] : []));
  }
  if (view.rename) rows.push(prompt("Rename to ", view.rename.input));
  if (view.editRoots) {
    const flow = view.editRoots;
    rows.push(text("Roots (first = primary):", "muted"));
    flow.roots.forEach((root, index) => rows.push({
      kind: "row",
      children: [text(index === flow.selected ? "▸ " : "  ", "accent"), text(index === 0 ? `${root} (primary)` : root, index === 0 ? "accent" : "fg")],
    }));
    if (flow.adding) rows.push(prompt("Add root ", flow.input));
  }
  const confirmed = items.find((project) => project.id === view.confirm);
  if (confirmed) rows.push(text(`Delete ${confirmed.name}? Enter confirms · Esc cancels`, "warning"));
  if (view.busy) {
    const seconds = view.busy.startedAt ? Math.max(0, Math.floor((now - view.busy.startedAt) / 1000)) : 0;
    rows.push({ kind: "row", children: [text(spinnerFrames[Math.floor(now / 100) % spinnerFrames.length]!, "accent"), text(` ${view.busy.label}… ${seconds}s`, "fg")] });
  }
  // A failed read of the Project list shows until a later read succeeds.
  const notice = view.notice ?? (projects.error ? { tone: "error" as const, text: `Refresh failed: ${projects.error}` } : undefined);
  if (notice) rows.push(text(notice.text, noticeColor(notice)));
  rows.push(text(hint(view), "muted"));
  return { kind: "column", children: rows };
}

type Projects = NonNullable<ExtensionContext["projects"]>;

export function registerProjectView(api: TuiExtensionApi): void {
  let view = closedView;
  /** The view was drawn before: a closed view starts again on the active Project. */
  let open = false;
  let pending: Pending | undefined;
  let sequence = 0;
  /** A Project just created: its row may reach the context after the result, so keep the highlight on it until then. */
  let created: string | undefined;

  /** Send one host command whose result `onResult` turns into the next state. */
  const run = (next: Pending, command: HostCommand, busy?: string): HandlerResult => {
    pending = next;
    if (busy) view = { ...view, busy: { label: busy, startedAt: 0 } };
    return { commands: [command] };
  };
  const token = (): string => `project-view-${++sequence}`;
  /** Close the view first, then switch or start a session (keys typed meanwhile go to the composer); a failure becomes a status notice. */
  const closeThen = (next: Pending, command: HostCommand): HandlerResult => {
    open = false;
    pending = next;
    return { invalidate: false, commands: [{ command: "ui.close" }, command] };
  };

  const key = (pressed: KeyEvent, projects: readonly Project[]): HandlerResult => {
    if (view.busy) return { invalidate: false };
    if (pressed.name === "tab" && (view.create?.step === "root" || view.editRoots?.adding)) {
      const input = view.create?.step === "root" ? view.create.input : view.editRoots?.input ?? "";
      return run({ kind: "complete" }, { command: "fs.complete", input, token: token() });
    }
    if (view.create) {
      const flow = view.create;
      const edit = editText(flow.input, pressed);
      if (edit.cancel) view = { ...view, create: undefined };
      else if (edit.text !== undefined) view = { ...view, create: { ...flow, input: edit.text } };
      else if (edit.commit && flow.step === "name") {
        const name = flow.input.trim();
        view = name ? { ...view, create: { step: "root", name, roots: [], input: "" } } : { ...view, notice: { tone: "info", text: "Name cannot be empty" } };
      } else if (edit.commit) {
        const root = flow.input.trim();
        if (root) view = { ...view, create: { ...flow, roots: [...flow.roots, root], input: "" } };
        else if (!flow.roots.length) view = { ...view, notice: { tone: "info", text: "At least one root is required" } };
        else {
          view = { ...view, create: undefined };
          return run({ kind: "create" }, { command: "project.create", name: flow.name, roots: flow.roots, token: token() }, "Creating");
        }
      }
      return { invalidate: true };
    }
    if (view.rename) {
      const flow = view.rename;
      const edit = editText(flow.input, pressed);
      if (edit.cancel) view = { ...view, rename: undefined };
      else if (edit.text !== undefined) view = { ...view, rename: { ...flow, input: edit.text } };
      else if (edit.commit) {
        const name = flow.input.trim();
        if (!name) view = { ...view, notice: { tone: "info", text: "Name cannot be empty" } };
        else {
          view = { ...view, rename: undefined };
          return run({ kind: "rename", name }, { command: "project.rename", id: flow.id, name, token: token() }, "Renaming");
        }
      }
      return { invalidate: true };
    }
    if (view.editRoots) {
      const flow = view.editRoots;
      if (flow.adding) {
        const edit = editText(flow.input, pressed);
        if (edit.cancel) view = { ...view, editRoots: { ...flow, adding: false, input: "" } };
        else if (edit.text !== undefined) view = { ...view, editRoots: { ...flow, input: edit.text } };
        else if (edit.commit) {
          const root = flow.input.trim();
          view = root
            ? { ...view, editRoots: { ...flow, roots: [...flow.roots, root], selected: flow.roots.length, adding: false, input: "" } }
            : { ...view, editRoots: { ...flow, adding: false, input: "" } };
        }
        return { invalidate: true };
      }
      if (pressed.name === "escape") view = { ...view, editRoots: undefined };
      else if (isEnter(pressed)) {
        view = { ...view, editRoots: undefined };
        return run({ kind: "roots" }, { command: "project.set_roots", id: flow.id, roots: flow.roots, token: token() }, "Saving roots");
      } else if ((pressed.name === "up" || pressed.name === "down") && !pressed.shift) {
        view = { ...view, editRoots: { ...flow, selected: pressed.name === "up" ? Math.max(0, flow.selected - 1) : Math.min(flow.roots.length - 1, flow.selected + 1) } };
      } else if ((pressed.name === "up" && flow.selected > 0) || (pressed.name === "down" && flow.selected < flow.roots.length - 1)) {
        const target = flow.selected + (pressed.name === "up" ? -1 : 1);
        const roots = [...flow.roots];
        const [root] = roots.splice(flow.selected, 1);
        roots.splice(target, 0, root!);
        view = { ...view, editRoots: { ...flow, roots, selected: target } };
      } else if (pressed.sequence === "a") view = { ...view, editRoots: { ...flow, adding: true, input: "" } };
      else if (pressed.sequence === "d") {
        if (flow.roots.length <= 1) view = { ...view, notice: { tone: "info", text: "A Project needs at least one root" } };
        else {
          const roots = flow.roots.filter((_, index) => index !== flow.selected);
          view = { ...view, editRoots: { ...flow, roots, selected: Math.min(flow.selected, roots.length - 1) } };
        }
      } else return { invalidate: false };
      return { invalidate: true };
    }
    if (view.confirm) {
      if (pressed.name === "escape") {
        view = { ...view, confirm: undefined };
        return { invalidate: true };
      }
      if (!isEnter(pressed)) return { invalidate: false };
      const id = view.confirm;
      view = { ...view, confirm: undefined };
      return run({ kind: "delete" }, { command: "project.delete", id, token: token() }, "Deleting");
    }
    if (pressed.name === "up" || pressed.name === "down") {
      view = move(view, projects, pressed.name === "up" ? -1 : 1);
      return { invalidate: true };
    }
    if (pressed.name === "escape") {
      open = false;
      return { invalidate: false, commands: [{ command: "ui.close" }] };
    }
    if (isEnter(pressed)) return view.highlighted ? closeThen({ kind: "switch" }, { command: "project.switch", id: view.highlighted, token: token() }) : { invalidate: false };
    if (pressed.ctrl || pressed.meta) return { invalidate: false };
    const highlighted = projects.find((project) => project.id === view.highlighted);
    switch (pressed.sequence) {
      case "n": view = { ...view, notice: undefined, create: { step: "name", name: "", roots: [], input: "" } }; break;
      case "t": return closeThen({ kind: "temporary" }, { command: "session.new_temporary", token: token() });
      case "r": if (!highlighted) return { invalidate: false }; view = { ...view, notice: undefined, rename: { id: highlighted.id, input: highlighted.name } }; break;
      case "e": if (!highlighted) return { invalidate: false }; view = { ...view, notice: undefined, editRoots: { id: highlighted.id, roots: [...highlighted.roots], selected: 0, adding: false, input: "" } }; break;
      case "d": if (!highlighted) return { invalidate: false }; view = { ...view, notice: undefined, confirm: highlighted.id }; break;
      default: return { invalidate: false };
    }
    return { invalidate: true };
  };

  const result = (answer: CommandResult, context: ExtensionContext): HandlerResult => {
    const done = pending;
    pending = undefined;
    view = { ...view, busy: undefined };
    if (!done) return { invalidate: true };
    if (done.kind === "complete") {
      const value = answer.value;
      const completion = typeof value === "object" && value !== null && "completion" in value && typeof value.completion === "string" ? value.completion : undefined;
      if (completion && view.create?.step === "root") view = { ...view, create: { ...view.create, input: completion } };
      else if (completion && view.editRoots?.adding) view = { ...view, editRoots: { ...view.editRoots, input: completion } };
      return { invalidate: true };
    }
    if (done.kind === "switch" || done.kind === "temporary") {
      if (answer.ok) return { invalidate: false };
      // As the view closed before the call, reopen it with the error.
      open = true;
      view = { ...closedView, highlighted: context.projects?.active ?? context.projects?.items[0]?.id, notice: { tone: "error", text: `${done.kind === "switch" ? "Switch failed" : "Temporary session failed"}: ${answer.error ?? "unknown error"}` } };
      return { commands: [{ command: "ui.open" }] };
    }
    if (!answer.ok) {
      const prefix = { create: "Create failed: ", rename: "Rename failed: ", roots: "Update failed: ", delete: "" }[done.kind];
      view = { ...view, notice: { tone: "error", text: `${prefix}${answer.error ?? "unknown error"}` } };
      return { invalidate: true };
    }
    switch (done.kind) {
      case "create": {
        const value = answer.value;
        const id = typeof value === "object" && value !== null && "id" in value && typeof value.id === "string" ? value.id : undefined;
        const name = typeof value === "object" && value !== null && "name" in value && typeof value.name === "string" ? value.name : "";
        created = id;
        view = { ...view, ...(id ? { highlighted: id } : {}), notice: { tone: "ok", text: `Created ${name}` } };
        break;
      }
      case "rename": view = { ...view, notice: { tone: "ok", text: `Renamed to ${done.name}` } }; break;
      case "roots": view = { ...view, notice: { tone: "ok", text: "Roots updated" } }; break;
      case "delete": view = { ...view, notice: { tone: "ok", text: "Deleted" } }; break;
    }
    return { invalidate: true };
  };

  api.registerPanel({
    id: "project-view",
    title: "Projects",
    replaces: "project_view",
    // The busy line's spinner and seconds advance with the host clock.
    refreshMs: 1000,
    render: ({ context, width, height, now }) => {
      const projects = context.projects ?? { ready: false, items: [] };
      if (!open) {
        open = true;
        view = { ...closedView, highlighted: projects.active ?? projects.items[0]?.id };
      }
      if (created && projects.items.some((project) => project.id === created)) created = undefined;
      if (!created) view = settle(view, projects.items);
      if (view.busy && !view.busy.startedAt) view = { ...view, busy: { ...view.busy, startedAt: now } };
      return render(view, projects, width, height, now);
    },
    onKey: (pressed, context) => key(pressed, context.projects?.items ?? []),
    onResult: result,
    onAction: (action) => {
      const id = actionId(action.data);
      if (!id) return { invalidate: false };
      return { invalidate: false, commands: [action.button === "right" ? { command: "project.menu", id } : { command: "project.switch", id }] };
    },
  });
}
