// Pure-logic unit tests for hya-extra/token-summary. `summary.ts` guards its
// stdio loop with `if (import.meta.main)`, so importing it here never spawns
// the JSON-RPC loop — only the exported transform/validation/reader pieces
// run.
import { describe, expect, test } from "bun:test";

import {
  CapabilityClient,
  type WireOutputSplit,
  type WireSessionUsageReport,
  type WireUsageTotals,
  type WireUsageTotalsReport,
  buildUsage,
  handleApiRequest,
  parseToolInput,
  parseUsageQuery,
  renderTable,
} from "./summary";

function totalsReport(
  overrides: Partial<WireUsageTotals>,
  split: WireOutputSplit,
): WireUsageTotalsReport {
  return {
    input: 0,
    cache_read: 0,
    cache_write: 0,
    output: 0,
    reasoning: 0,
    reasoning_unknown_output: 0,
    rounds: 0,
    legacy_messages: 0,
    ...overrides,
    split,
  };
}

// Anthropic-shaped: high volume, thinking split unknown (`reasoning_unknown`
// on every round, so `split.unknown == output`).
const ANTHROPIC = totalsReport(
  { input: 500, cache_read: 50, cache_write: 20, output: 200, reasoning_unknown_output: 200, rounds: 3 },
  { thinking: 0, visible: 0, unknown: 200 },
);

// A FakeLlm-shaped model: lower volume, thinking split fully known.
const FAKE_MODEL = totalsReport(
  { input: 100, output: 40, reasoning: 10, rounds: 2 },
  { thinking: 10, visible: 30, unknown: 0 },
);

// The merge of the two rows above (same shape `UsageTotals::merge` produces).
const MERGED_TOTAL = totalsReport(
  { input: 600, cache_read: 50, cache_write: 20, output: 240, reasoning: 10, reasoning_unknown_output: 200, rounds: 5 },
  { thinking: 10, visible: 30, unknown: 200 },
);

const REPORT: WireSessionUsageReport = {
  session: "sess-root",
  scope: "tree",
  root: "sess-root",
  sessions: [
    {
      session: "sess-root",
      agent: "build",
      usage: { by_model: { "fake/model": FAKE_MODEL }, by_purpose: {}, total: FAKE_MODEL },
    },
    {
      session: "sess-child",
      parent: "sess-root",
      agent: "general",
      usage: {
        by_model: { "anthropic/claude-opus-5-5": ANTHROPIC },
        by_purpose: {},
        total: ANTHROPIC,
      },
    },
  ],
  total: {
    by_model: { "fake/model": FAKE_MODEL, "anthropic/claude-opus-5-5": ANTHROPIC },
    by_purpose: {},
    total: MERGED_TOTAL,
  },
};

describe("buildUsage", () => {
  test("carries session/scope/generated_by through unchanged", () => {
    const summary = buildUsage(REPORT);
    expect(summary.session).toBe("sess-root");
    expect(summary.scope).toBe("tree");
    expect(summary.generated_by).toBe("hya-extra/token-summary");
  });

  test("sorts models by prompt+output total descending, then name", () => {
    const summary = buildUsage(REPORT);
    // anthropic: prompt_total 570 + output 200 = 770; fake/model: 100 + 40 = 140.
    expect(summary.models.map((row) => row.model)).toEqual(["anthropic/claude-opus-5-5", "fake/model"]);
  });

  test("maps cache_write to cache_creation", () => {
    const summary = buildUsage(REPORT);
    const anthropic = summary.models.find((row) => row.model === "anthropic/claude-opus-5-5");
    expect(anthropic?.cache_creation).toBe(20);
    expect(anthropic?.cache_read).toBe(50);
  });

  test("thinking/visible_output are null exactly when the split has any unknown share", () => {
    const summary = buildUsage(REPORT);
    const anthropic = summary.models.find((row) => row.model === "anthropic/claude-opus-5-5");
    expect(anthropic?.thinking).toBeNull();
    expect(anthropic?.visible_output).toBeNull();
    expect(anthropic?.unsplit_output).toBe(200);

    const fake = summary.models.find((row) => row.model === "fake/model");
    expect(fake?.thinking).toBe(10);
    expect(fake?.visible_output).toBe(30);
    expect(fake?.unsplit_output).toBe(0);
    expect(fake?.prompt_total).toBe(100);
  });

  test("the grand total follows the same rules as a model row", () => {
    const summary = buildUsage(REPORT);
    expect(summary.total.rounds).toBe(5);
    expect(summary.total.input).toBe(600);
    expect(summary.total.cache_creation).toBe(20);
    expect(summary.total.thinking).toBeNull();
    expect(summary.total.unsplit_output).toBe(200);
  });

  test("emits one row per session, preserving parent/agent and per-session models", () => {
    const summary = buildUsage(REPORT);
    expect(summary.sessions).toHaveLength(2);
    expect(summary.sessions[0]).toMatchObject({ session: "sess-root", agent: "build" });
    expect(summary.sessions[0].parent).toBeUndefined();
    expect(summary.sessions[0].models.map((row) => row.model)).toEqual(["fake/model"]);
    expect(summary.sessions[1]).toMatchObject({
      session: "sess-child",
      parent: "sess-root",
      agent: "general",
    });
    expect(summary.sessions[1].models.map((row) => row.model)).toEqual(["anthropic/claude-opus-5-5"]);
  });

  test("truncated is included only when true", () => {
    expect(buildUsage(REPORT).truncated).toBeUndefined();
    expect(buildUsage({ ...REPORT, truncated: true }).truncated).toBe(true);
    expect(buildUsage({ ...REPORT, truncated: false }).truncated).toBeUndefined();
  });
});

describe("renderTable", () => {
  test("renders a model row, an unknown-split row with dashes, and a totals row", () => {
    const table = renderTable(buildUsage(REPORT), "tree");
    expect(table).toContain(
      "| anthropic/claude-opus-5-5 | 500 | 20 | 50 | 200 | — | — | 3 |",
    );
    expect(table).toContain("| fake/model | 100 | 0 | 0 | 40 | 10 | 30 | 2 |");
    expect(table).toContain("| **total** | 600 | 20 | 50 | 240 | — | — | 5 |");
  });

  test("lists one line per subagent session when scope != session", () => {
    const table = renderTable(buildUsage(REPORT), "tree");
    expect(table).toContain("Sessions:");
    expect(table).toContain("sess-root");
    expect(table).toContain("sess-child (general)");
  });

  test("omits the session list at scope session", () => {
    const single: WireSessionUsageReport = { ...REPORT, scope: "session", sessions: [REPORT.sessions[0]] };
    const table = renderTable(buildUsage(single), "session");
    expect(table).not.toContain("Sessions:");
  });
});

describe("parseUsageQuery", () => {
  test("defaults to tree", () => {
    expect(parseUsageQuery({})).toEqual({ ok: true, scope: "tree" });
  });

  test("accepts scope=session", () => {
    expect(parseUsageQuery({ scope: "session" })).toEqual({ ok: true, scope: "session" });
  });

  test("rejects scope=root (the usage endpoint may only read its own session or its tree)", () => {
    expect(parseUsageQuery({ scope: "root" }).ok).toBe(false);
  });

  test("rejects an unrecognized scope value", () => {
    expect(parseUsageQuery({ scope: "bogus" }).ok).toBe(false);
  });

  test("rejects the removed by key", () => {
    expect(parseUsageQuery({ by: "session" }).ok).toBe(false);
  });

  test("rejects an unknown query key", () => {
    expect(parseUsageQuery({ scope: "tree", bogus: "1" }).ok).toBe(false);
  });
});

describe("parseToolInput", () => {
  test("defaults to tree/table", () => {
    expect(parseToolInput(undefined)).toEqual({ ok: true, scope: "tree", format: "table" });
    expect(parseToolInput(null)).toEqual({ ok: true, scope: "tree", format: "table" });
    expect(parseToolInput({})).toEqual({ ok: true, scope: "tree", format: "table" });
  });

  test("accepts scope=root (unlike the usage endpoint, a tool call may read the whole spawn tree root)", () => {
    expect(parseToolInput({ scope: "root" })).toEqual({ ok: true, scope: "root", format: "table" });
  });

  test("accepts format=json", () => {
    expect(parseToolInput({ format: "json" })).toEqual({ ok: true, scope: "tree", format: "json" });
  });

  test("rejects an invalid scope", () => {
    expect(parseToolInput({ scope: "bogus" }).ok).toBe(false);
  });

  test("rejects an invalid format", () => {
    expect(parseToolInput({ format: "xml" }).ok).toBe(false);
  });

  test("rejects an unknown input field", () => {
    expect(parseToolInput({ scope: "tree", extra: true }).ok).toBe(false);
  });

  test("rejects a non-object input", () => {
    expect(parseToolInput("nope").ok).toBe(false);
    expect(parseToolInput(42).ok).toBe(false);
    expect(parseToolInput([1, 2]).ok).toBe(false);
  });
});

describe("CapabilityClient (the id-dispatching reader)", () => {
  test("classifies a reply by id and leaves a method-bearing host request untouched", async () => {
    const written: Record<string, unknown>[] = [];
    const cap = new CapabilityClient((frame) => written.push(frame));

    const pending = cap.call("cap-token", "sess-1", "call-1", "session.usage", { scope: "tree" });
    expect(written).toHaveLength(1);
    const outgoingId = written[0].id;

    // A brand-new host request interleaves before our capability reply
    // arrives on the same stdio connection; a naive sequential reader that
    // only ever awaited the next line in request order would stall here.
    // handleFrame must recognize it as a request (it carries `method`) and
    // hand it back to the caller instead of consuming it.
    const hostRequest = { jsonrpc: "2.0", id: 999, method: "api/request", params: {} };
    expect(cap.handleFrame(hostRequest)).toBe(false);

    // The capability reply now arrives, matched purely by id.
    const reply = { jsonrpc: "2.0", id: outgoingId, result: { session: "sess-1" } };
    expect(cap.handleFrame(reply)).toBe(true);

    await expect(pending).resolves.toEqual({ session: "sess-1" });
  });

  test("two concurrent calls resolve independently, even answered out of order", async () => {
    const written: Record<string, unknown>[] = [];
    const cap = new CapabilityClient((frame) => written.push(frame));

    const first = cap.call("cap", "s1", "c1", "session.usage", { scope: "tree" });
    const second = cap.call("cap", "s1", "c2", "session.usage", { scope: "session" });
    expect(written).toHaveLength(2);

    // Reply to the second call first: a sequential (non-id-keyed) reader
    // would incorrectly resolve the FIRST pending promise with this result.
    cap.handleFrame({ jsonrpc: "2.0", id: written[1].id, result: { marker: "second" } });
    cap.handleFrame({ jsonrpc: "2.0", id: written[0].id, result: { marker: "first" } });

    await expect(first).resolves.toEqual({ marker: "first" });
    await expect(second).resolves.toEqual({ marker: "second" });
  });

  test("rejects the pending call on a JSON-RPC error reply", async () => {
    const written: Record<string, unknown>[] = [];
    const cap = new CapabilityClient((frame) => written.push(frame));
    const pending = cap.call("cap-token", "sess-1", "call-1", "session.usage", { scope: "root" });
    const outgoingId = written[0].id;
    cap.handleFrame({
      jsonrpc: "2.0",
      id: outgoingId,
      error: { code: -32602, message: "scope `root` is not available" },
    });
    await expect(pending).rejects.toThrow("scope `root` is not available");
  });

  test("an unrelated reply id is swallowed, not left pending forever", () => {
    const cap = new CapabilityClient(() => {});
    expect(cap.handleFrame({ jsonrpc: "2.0", id: 4242, result: {} })).toBe(true);
  });
});

describe("handleApiRequest", () => {
  const params = (overrides: Record<string, unknown> = {}) => ({
    api: "usage",
    method: "GET",
    path: "/usage",
    path_params: {},
    query: {},
    body: null,
    session: "sess-root",
    call: "call-1",
    host_capability: "cap-token",
    ...overrides,
  });

  test("answers 200 with the usage body read through session.usage", async () => {
    const written: Record<string, unknown>[] = [];
    const cap = new CapabilityClient((frame) => written.push(frame));
    const pending = handleApiRequest(params({ query: { scope: "session" } }), cap);
    expect(written).toHaveLength(1);
    expect(written[0].params).toMatchObject({
      capability: "cap-token",
      session: "sess-root",
      call: "call-1",
      method: "session.usage",
      params: { scope: "session" },
    });
    cap.handleFrame({ jsonrpc: "2.0", id: written[0].id, result: REPORT });
    const reply = await pending;
    expect(reply.status).toBe(200);
    expect(reply.body).toEqual(buildUsage(REPORT));
  });

  test("a bad query is the caller's 400, without a capability call", async () => {
    const written: Record<string, unknown>[] = [];
    const cap = new CapabilityClient((frame) => written.push(frame));
    for (const query of [{ scope: "root" }, { by: "model" }]) {
      const reply = await handleApiRequest(params({ query }), cap);
      expect(reply.status).toBe(400);
      expect(reply.body).toHaveProperty("error");
    }
    expect(written).toHaveLength(0);
  });

  test("an unknown endpoint or method is refused defensively", async () => {
    const cap = new CapabilityClient(() => {});
    expect((await handleApiRequest(params({ api: "other" }), cap)).status).toBe(404);
    expect((await handleApiRequest(params({ method: "POST" }), cap)).status).toBe(405);
    expect((await handleApiRequest(params({ session: undefined }), cap)).status).toBe(500);
  });

  test("a failed capability read is a 500 with the reason", async () => {
    const written: Record<string, unknown>[] = [];
    const cap = new CapabilityClient((frame) => written.push(frame));
    const pending = handleApiRequest(params(), cap);
    cap.handleFrame({
      jsonrpc: "2.0",
      id: written[0].id,
      error: { code: -32001, message: "capability expired" },
    });
    const reply = await pending;
    expect(reply.status).toBe(500);
    expect(reply.body).toEqual({ error: "capability expired" });
  });
});
