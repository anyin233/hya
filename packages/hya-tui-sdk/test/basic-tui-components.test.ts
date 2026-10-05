// hya/basic-tui-components (bundles/first-party/basic-tui-components): the
// panes the TUI no longer builds in, run the way the TUI runs them — bundled
// with the SDK and driven over the wire inside the QuickJS VM.
import { beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { ExtensionVm, bundleExtension } from "../src/host";
import type { CommandResult, ExtensionContext, HostCommand, KeyEvent, TuiMethod, TuiMethodMap } from "../src/protocol";
import type { RenderNode } from "../src/render";

const entry = join(import.meta.dir, "../../../bundles/first-party/basic-tui-components/tui/main.ts");
let script = "";
beforeAll(async () => { script = await bundleExtension(entry); });

type Session = NonNullable<ExtensionContext["sessions"]>["items"][number];
const session = (id: string, extra: Partial<Session> = {}): Session => ({ id, title: id.toUpperCase(), agent: "build", temporary: false, archived: false, busy: false, waiting: false, created: 0, ...extra });
const status: NonNullable<ExtensionContext["status"]> = {
  ready: true, mode: { text: "manual", tone: "strong" },
  session: { id: "s1", title: "Work", agent: "build", model: "acme/m1:high", modelShort: "m1:high", messages: 3, workdir: "/home/me/work", context: { percent: 42, tokens: 42_000, limit: 100_000 }, tokens: 1234 },
  branch: "main", items: [], server: "127.0.0.1:8080", versions: { tui: "0.44.6", backend: "0.45.3" }, connection: "connected",
};
const base: ExtensionContext = {
  terminal: { columns: 120, rows: 40 },
  sessions: { ready: true, selected: "b", items: [session("a", { created: 1, members: [{ child: "b", handle: "hya-scout-skade" }] }), session("b", { parent: "a", created: 2, busy: true }), session("c", { created: 3, waiting: true }), session("t", { created: 4, temporary: true })] },
  projects: { ready: true, active: "p1", items: [{ id: "p1", name: "One", roots: ["/one"], busy: false, sessionCount: 2 }, { id: "p2", name: "Two", roots: ["/two"], busy: true, sessionCount: 0 }] },
  todos: [{ status: "pending", content: "plan" }, { status: "in_progress", content: "build" }, { status: "blocked", content: "wait" }, { status: "completed", content: "ship" }],
  status,
};

/** One VM with the bundle activated against `context`, and typed calls into it. */
async function open(context: ExtensionContext = base) {
  const vm = await ExtensionVm.create(script);
  let id = 0;
  const call = async <M extends TuiMethod>(method: M, params: TuiMethodMap[M]["params"]): Promise<TuiMethodMap[M]["result"]> => {
    id += 1;
    const response: { result?: TuiMethodMap[M]["result"]; error?: { message: string } } = JSON.parse(await vm.handle(JSON.stringify({ jsonrpc: "2.0", id, method, params }), id));
    if (response.error || response.result === undefined) throw new Error(response.error?.message ?? `${method}: no result`);
    return response.result;
  };
  await call("tui/initialize", { api_version: 1, sdk_version: "1.0.0", extension_id: "hya/basic-tui-components", permissions: [] });
  await call("tui/activate", { context });
  const surface = (panel: string) => ({ kind: "panel" as const, id: panel });
  return {
    vm,
    context: (next: ExtensionContext) => call("tui/context", { context: next }),
    render: (panel: string, width = 40, height = 20, now = 0) => call("tui/render", { surface: surface(panel), width, height, now }).then((value) => value.root),
    key: (panel: string, name: string, extra: Partial<KeyEvent> = {}) => call("tui/key", { surface: surface(panel), key: { name, sequence: name.length === 1 ? name : "", ctrl: false, shift: false, meta: false, ...extra } }),
    type: async (panel: string, input: string) => { for (const character of input) await call("tui/key", { surface: surface(panel), key: { name: character, sequence: character, ctrl: false, shift: false, meta: false } }); },
    result: (panel: string, result: CommandResult) => call("tui/command_result", { surface: surface(panel), result }),
    action: (panel: string, data: { id: string }, button: "left" | "right" = "left") => call("tui/action", { surface: surface(panel), action: { name: "switch", data, button } }),
  };
}

/** The text of every line of a rendered tree: a row's segments joined, one entry per text or row. */
function lines(node: RenderNode | null | undefined): string[] {
  if (!node) return [];
  if (node.kind === "text") return [node.text];
  if (node.kind === "row") return [node.children.map((child: RenderNode) => child.kind === "text" ? child.text : "").join("")];
  if (node.kind === "column" || node.kind === "box") return node.children.flatMap(lines);
  return [];
}

/** Every text node with its color, depth-first. */
function texts(node: RenderNode | null | undefined): { text: string; color?: string }[] {
  if (!node) return [];
  if (node.kind === "text") return [{ text: node.text, ...(node.style?.color ? { color: node.style.color } : {}) }];
  return "children" in node ? node.children.flatMap(texts) : [];
}

const tokenOf = (commands: readonly HostCommand[] | undefined): string => {
  const command = commands?.find((candidate) => "token" in candidate);
  return command && "token" in command && command.token ? command.token : "";
};

test("Sessions: a stable tree with numbering, member handles, markers, rules, and the Temporary heading", async () => {
  const panel = await open();
  expect(lines(await panel.render("sessions", 30))).toEqual([
    "  1. A",
    "   build",
    "▸  ↳ 1.1 hya-scout-skade · ru…",
    "──────────────────────────────",
    "  2. C",
    "   build · ◌ waiting",
    "— Temporary —",
    "──────────────────────────────",
    "  3. T",
    "   build",
  ]);
  await panel.context({ ...base, sessions: { ready: false, items: [] } });
  expect(lines(await panel.render("sessions"))).toEqual(["Loading…"]);
  panel.vm.dispose();
});

test("Sessions: a click opens the session, a right click asks for its menu", async () => {
  const panel = await open();
  expect((await panel.action("sessions", { id: "b" })).commands).toEqual([{ command: "session.open", id: "b", token: "open" }]);
  expect((await panel.action("sessions", { id: "b" }, "right")).commands).toEqual([{ command: "session.menu", id: "b" }]);
  expect((await panel.result("sessions", { token: "open", ok: false, error: "not_found: gone" })).notice).toBe("Open failed: not_found: gone");
  panel.vm.dispose();
});

test("Todos: one line per item with its glyph and color", async () => {
  const panel = await open();
  expect(lines(await panel.render("todos"))).toEqual(["○ plan", "◐ build", "✗ wait", "✓ ship"]);
  expect(texts(await panel.render("todos")).filter((_, index) => index % 2 === 0).map((node) => node.color)).toEqual(["muted", "accent", "muted", "success"]);
  await panel.context({ ...base, todos: [] });
  expect(lines(await panel.render("todos"))).toEqual(["No todos yet"]);
  panel.vm.dispose();
});

test("Projects: rows with highlight, busy marker, and count; keys move, switch, and release", async () => {
  const panel = await open();
  expect(lines(await panel.render("projects", 20))).toEqual(["▸   One (2)", "────────────────────", "  ● Two (0)"]);
  expect((await panel.key("projects", "down")).invalidate).toBe(true);
  expect(lines(await panel.render("projects", 20))[2]).toBe("▸ ● Two (0)");
  expect((await panel.key("projects", "return")).commands).toEqual([{ command: "ui.release" }, { command: "project.switch", id: "p2", token: "switch" }]);
  expect((await panel.key("projects", "escape")).commands).toEqual([{ command: "ui.release" }]);
  expect((await panel.action("projects", { id: "p1" }, "right")).commands).toEqual([{ command: "project.menu", id: "p1" }]);
  panel.vm.dispose();
});

test("Project view: create asks a name then roots, runs project.create, and shows the result", async () => {
  const panel = await open();
  expect(lines(await panel.render("project-view", 60, 30)).at(-1)).toContain("n new");
  await panel.key("project-view", "n");
  await panel.type("project-view", "New");
  await panel.key("project-view", "return");
  await panel.type("project-view", "/new/root");
  await panel.key("project-view", "return");
  const create = (await panel.key("project-view", "return")).commands;
  expect(create).toEqual([{ command: "project.create", name: "New", roots: ["/new/root"], token: tokenOf(create) }]);
  // Busy: the spinner and the seconds follow the host clock.
  await panel.render("project-view", 60, 30, 10_000);
  expect(lines(await panel.render("project-view", 60, 30, 13_500))).toContain("⠴ Creating… 3s");
  await panel.result("project-view", { token: tokenOf(create), ok: true, value: { id: "p3", name: "New" } });
  await panel.context({ ...base, projects: { ...base.projects!, items: [...base.projects!.items, { id: "p3", name: "New", roots: ["/new/root"], busy: false }] } });
  const shown = lines(await panel.render("project-view", 60, 30));
  expect(shown).toContain("Created New");
  expect(shown.find((row) => row.startsWith("▸ "))).toContain("New");
  panel.vm.dispose();
});

test("Project view: a failure is a notice; a switch closes the view first and reopens it with the error", async () => {
  const panel = await open();
  await panel.render("project-view", 60, 30);
  await panel.key("project-view", "d");
  const remove = (await panel.key("project-view", "return")).commands;
  expect(remove).toEqual([{ command: "project.delete", id: "p1", token: tokenOf(remove) }]);
  await panel.result("project-view", { token: tokenOf(remove), ok: false, error: "failed_precondition: a session is live" });
  expect(lines(await panel.render("project-view", 60, 30))).toContain("failed_precondition: a session is live");
  const switching = (await panel.key("project-view", "return")).commands;
  expect(switching).toEqual([{ command: "ui.close" }, { command: "project.switch", id: "p1", token: tokenOf(switching) }]);
  expect((await panel.result("project-view", { token: tokenOf(switching), ok: false, error: "unavailable: fetch failed" })).commands).toEqual([{ command: "ui.open" }]);
  expect(lines(await panel.render("project-view", 60, 30))).toContain("Switch failed: unavailable: fetch failed");
  await panel.context({ ...base, projects: { ...base.projects!, error: "unavailable: remote backend is offline" } });
  await panel.key("project-view", "escape");
  expect(lines(await panel.render("project-view", 60, 30))).toContain("Refresh failed: unavailable: remote backend is offline");
  panel.vm.dispose();
});

test("Project view: Tab completes a root path through fs.complete", async () => {
  const panel = await open();
  await panel.render("project-view", 60, 30);
  await panel.key("project-view", "n");
  await panel.type("project-view", "X");
  await panel.key("project-view", "return");
  await panel.type("project-view", "/wo");
  const complete = (await panel.key("project-view", "tab")).commands;
  expect(complete).toEqual([{ command: "fs.complete", input: "/wo", token: tokenOf(complete) }]);
  await panel.result("project-view", { token: tokenOf(complete), ok: true, value: { candidates: ["/work"], completion: "/work" } });
  expect(lines(await panel.render("project-view", 60, 30)).some((row) => row.startsWith("Root 1 (primary) /work▏"))).toBe(true);
  panel.vm.dispose();
});

test("Context: ordered labelled rows; occupancy warns at 80% and alarms at 95%", async () => {
  const panel = await open();
  expect(lines(await panel.render("context", 40))).toEqual([
    "Mode     manual", "Session  Work", "Agent    build", "Model    acme/m1:high", "Messages 3",
    "Context  42% · 42k/100k", "Tokens   1.2k", "Dir      /home/me/work", "Branch   main",
    "Server   127.0.0.1:8080", "Version  0.44.6/0.45.3",
  ]);
  const tone = async (percent: number) => {
    await panel.context({ ...base, status: { ...status, session: { ...status.session!, context: { percent, tokens: percent * 1000, limit: 100_000 } } } });
    return texts(await panel.render("context", 40)).find((node) => node.text.startsWith(`${percent}%`))?.color;
  };
  expect([await tone(79), await tone(80), await tone(95)]).toEqual(["fg", "warning", "error"]);
  panel.vm.dispose();
});

test("Context line: segments by priority while they fit", async () => {
  const panel = await open();
  expect(lines(await panel.render("context-line", 200, 1))).toEqual(["mode manual · Work · 0.44.6/0.45.3 · m1:high · ctx 42% · build · 1.2k tok · /home/me/work · ⎇ main · 3 msgs · 127.0.0.1:8080"]);
  // `0.44.6/0.45.3` (priority 1) no longer fits; the shorter `m1:high` (priority 2) still does.
  expect(lines(await panel.render("context-line", 30, 1))).toEqual(["mode manual · Work · m1:high"]);
  panel.vm.dispose();
});
