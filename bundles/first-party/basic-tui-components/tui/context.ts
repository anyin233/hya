/**
 * The session context, shown in exactly one place at a time: the Context
 * pane (one labelled row per field) while the right sidebar shows, else the
 * Conversation's top status line (`context-line`: compact segments, the
 * least important dropped first when the line is too narrow).
 */
import type { ExtensionContext, RenderNode, TuiExtensionApi } from "@hya/tui-sdk";
import { formatTokens, text, truncate, truncateStart } from "./text";

/**
 * How a field is colored: `plain` is the box's text color and the status
 * line's muted color; `strong` is the text color on both; the rest are the
 * theme colors of the same name.
 */
type Tone = "plain" | "strong" | "accent" | "warning" | "error";

interface Field {
  /** Box row label (`Session`, `Mode`, …). */
  readonly label: string;
  readonly value: string;
  /** Status line segment (`mode manual`, `ctx 42%`, `⎇ main`, …). */
  readonly short: string;
  readonly tone: Tone;
  /** Which end of `value` the box cuts: paths and ids lose their start (`…/work`), names their end. */
  readonly cut: "start" | "end";
  /** Drop order on a narrow status line: the highest number drops first; 0 is never dropped. */
  readonly priority: number;
}

/** The box's label column (`Messages` plus one space). */
const labelWidth = 9;
/** The status line's directory segment keeps this many columns of the path's tail. */
const directoryWidth = 24;
/** Context percent from which occupancy warns (warning color) and alarms (error color). */
const contextWarnPercent = 80;
const contextAlarmPercent = 95;

/**
 * The ordered fields: Vim (when on), Mode, Session, Forked, Agent, Model,
 * Messages, Context, Tokens, Dir, Branch, Todos, extension status items,
 * Server, WebUI, Version, Backend. Fields with no data are omitted.
 */
function contextFields(status: NonNullable<ExtensionContext["status"]>): Field[] {
  const fields: Field[] = [];
  const add = (field: Omit<Field, "tone" | "cut"> & Partial<Pick<Field, "tone" | "cut">>): void => {
    fields.push({ tone: "plain", cut: "start", ...field });
  };
  if (status.vim) {
    const mode = status.vim.normal ? "NORMAL" : "INSERT";
    const pending = status.vim.pending ? ` ${status.vim.pending}` : "";
    add({ label: "Vim", value: `${mode}${pending}`, short: `-- ${mode} --${pending}`, tone: status.vim.normal ? "accent" : "plain", cut: "end", priority: 0 });
  }
  add({ label: "Mode", value: status.mode.text, short: `mode ${status.mode.text}`, tone: status.mode.tone, cut: "end", priority: 0 });
  const session = status.session;
  const title = session ? session.title || session.id : status.ready ? "none" : "connecting…";
  add({ label: "Session", value: title, short: title, tone: "accent", priority: 1 });
  if (session) {
    if (session.forked) add({ label: "Forked", value: session.forked.replace(/^forked /, ""), short: session.forked, cut: "end", priority: 9 });
    add({ label: "Agent", value: session.agent, short: session.agent, priority: 4 });
    add({ label: "Model", value: session.model || "default", short: session.modelShort || "default", cut: "end", priority: 2 });
    add({ label: "Messages", value: String(session.messages), short: `${session.messages} msg${session.messages === 1 ? "" : "s"}`, priority: 8 });
    const usage = session.context;
    if (usage) {
      const tone = usage.percent >= contextAlarmPercent ? "error" : usage.percent >= contextWarnPercent ? "warning" : "plain";
      add({ label: "Context", value: `${usage.percent}% · ${formatTokens(usage.tokens)}/${formatTokens(usage.limit)}`, short: `ctx ${usage.percent}%`, tone, priority: 3 });
    }
    if (session.tokens !== undefined) add({ label: "Tokens", value: formatTokens(session.tokens), short: `${formatTokens(session.tokens)} tok`, priority: 5 });
    if (session.workdir) add({ label: "Dir", value: session.workdir, short: truncateStart(session.workdir, directoryWidth), priority: 6 });
  }
  if (status.branch) add({ label: "Branch", value: status.branch, short: `⎇ ${status.branch}`, priority: 7 });
  if (status.todos) add({ label: "Todos", value: `${status.todos.done}/${status.todos.total}`, short: `Todos ${status.todos.done}/${status.todos.total}`, priority: 7 });
  for (const item of status.items) add({ label: item.label, value: item.text, short: item.text, priority: item.priority });
  add({ label: "Server", value: status.server, short: status.server, priority: 9 });
  if (status.web) add({ label: "WebUI", value: status.web.host ?? "unavailable", short: status.web.label, tone: status.web.host ? "plain" : "warning", priority: 4 });
  const version = `${status.versions.tui}/${status.versions.backend || "unknown"}`;
  add({ label: "Version", value: version, short: version, priority: 1 });
  if (status.connection === "stopped") add({ label: "Backend", value: "stopped", short: "backend stopped", tone: "error", priority: 2 });
  else if (status.connection === "disconnected") add({ label: "Backend", value: "reconnecting", short: "reconnecting", tone: "warning", priority: 2 });
  return fields;
}

/** Box value color: plain and strong values in the text color, the rest in their theme color. */
const boxColor = (tone: Tone): string => tone === "plain" || tone === "strong" ? "fg" : tone;

/** The Context pane: one row per field, the value cut to `width` minus the label column. */
function contextBox(fields: readonly Field[], width: number): RenderNode {
  const room = Math.max(4, width - labelWidth);
  return {
    kind: "column",
    children: fields.map((field): RenderNode => ({
      kind: "row",
      children: [
        text(field.label.padEnd(labelWidth), "muted"),
        text(field.cut === "start" ? truncateStart(field.value, room) : truncate(field.value, room), boxColor(field.tone)),
      ],
    })),
  };
}

/** The status line: segments by priority while they fit; priority 0 always stays. */
function contextLine(fields: readonly Field[], width: number): string {
  const kept: string[] = [];
  let room = Math.max(1, width);
  for (const field of [...fields].sort((left, right) => left.priority - right.priority)) {
    const separator = kept.length ? 3 : 0;
    if (field.priority > 0 && field.short.length + separator > room) continue;
    kept.push(field.short);
    room -= field.short.length + separator;
  }
  return kept.join(" · ");
}

export function registerContext(api: TuiExtensionApi): void {
  api.registerPanel({
    id: "context",
    title: "Context",
    replaces: "context",
    render: ({ context, width }) => context.status ? contextBox(contextFields(context.status), width) : null,
  });
  api.registerPanel({
    id: "context-line",
    title: "Context",
    replaces: "context_line",
    render: ({ context, width }) => context.status ? text(truncate(contextLine(contextFields(context.status), width), width), "muted") : null,
  });
}
