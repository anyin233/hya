// Unit tests for the pure routing logic in router.ts. Run with `bun test` from
// this directory. Not declared in bundle.yaml, so it is never packaged.
import { describe, expect, test } from "bun:test";

import {
  LruMap,
  Router,
  buildJevRequest,
  handleMessage,
  jevClassifier,
  latestUserMessage,
  parseConfig,
  parseJevAnswer,
  type ChatParams,
  type RouterConfig,
} from "./router.ts";

const TIERS = [
  { name: "easy", model: "fake/tier-easy", criteria: "Trivial lookups" },
  { name: "medium", model: "fake/tier-medium", criteria: "Normal feature work" },
  { name: "hard", model: "fake/tier-hard", criteria: "Cross-cutting design" },
];

function config(overrides: Record<string, unknown> = {}): RouterConfig {
  const raw = {
    jev: { api_key: "k", ...(overrides.jev as object) },
    route: { default_tier: "medium", ...(overrides.route as object) },
    tiers: TIERS,
  };
  const result = parseConfig(raw);
  if (!result.ok) throw new Error(result.error);
  return result.config;
}

function user(id: string, text: string) {
  return { role: "user", id, parts: [{ type: "text", id: `${id}-p`, text }] };
}

function assistantToolRound(id: string) {
  return {
    role: "assistant",
    id,
    agent: "build",
    model: "fake/model",
    parts: [
      {
        type: "tool",
        id: `${id}-t`,
        call_id: `${id}-c`,
        name: "read",
        state: { status: "completed", input: {}, output: "ok", time_ms: 1 },
      },
    ],
  };
}

function params(
  session: string,
  root: string,
  messages: unknown[],
  model = "fake/model",
): ChatParams {
  return {
    session,
    root_session: root,
    agent: "build",
    message: "m",
    request: { model, system: "You are hya", messages, tools: [], headers: {} },
  };
}

/** A scripted classifier that records how often it was asked. */
function scripted(answers: Array<number | null>) {
  const calls: ChatParams[] = [];
  const classify = async (p: ChatParams) => {
    calls.push(p);
    return answers.length > 0 ? (answers.shift() as number | null) : null;
  };
  return { calls, classify };
}

const quiet = () => {};

describe("parseConfig", () => {
  test("applies documented defaults", () => {
    const result = parseConfig({ jev: { api_key: " key \n" }, tiers: TIERS });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.config.jev).toEqual({
      endpoint: "https://api.typesafe.ai/v1/systemone",
      model: "jev-latest",
      apiKey: "key",
      timeoutMs: 2000,
      minConfidence: 0.5,
    });
    expect(result.config.route).toEqual({
      from: [],
      defaultTier: 2,
      stickiness: "chain",
      escalate: false,
    });
    expect(result.config.tiers.map((t) => t.name)).toEqual(["easy", "medium", "hard"]);
  });

  test("reads and trims api_key_file through the injected reader", () => {
    const result = parseConfig(
      { jev: { api_key_file: "/secrets/jev" }, tiers: TIERS },
      (path) => (path === "/secrets/jev" ? "file-key\n" : ""),
    );
    expect(result.ok && result.config.jev.apiKey).toBe("file-key");
  });

  test("tolerates unrelated top-level keys such as hya's own agents leaf", () => {
    expect(parseConfig({ agents: {}, jev: { api_key: "k" }, tiers: TIERS }).ok).toBe(true);
  });

  const invalid: Array<[string, unknown]> = [
    ["not a mapping", "text"],
    ["missing tiers", { jev: { api_key: "k" } }],
    ["empty tiers", { jev: { api_key: "k" }, tiers: [] }],
    ["duplicate tier", { jev: { api_key: "k" }, tiers: [TIERS[0], TIERS[0]] }],
    ["tier without model", { jev: { api_key: "k" }, tiers: [{ name: "a", criteria: "x" }] }],
    ["unknown default tier", { jev: { api_key: "k" }, route: { default_tier: "x" }, tiers: TIERS }],
    ["no api key", { jev: {}, tiers: TIERS }],
    ["both api keys", { jev: { api_key: "k", api_key_file: "/k" }, tiers: TIERS }],
    ["relative key file", { jev: { api_key_file: "k.txt" }, tiers: TIERS }],
    ["bad stickiness", { jev: { api_key: "k" }, route: { stickiness: "turn" }, tiers: TIERS }],
    ["typo in jev", { jev: { api_key: "k", timeout: 5 }, tiers: TIERS }],
    ["confidence out of range", { jev: { api_key: "k", min_confidence: 2 }, tiers: TIERS }],
    ["timeout beyond the hook budget", { jev: { api_key: "k", timeout_ms: 60000 }, tiers: TIERS }],
    ["from not a list", { jev: { api_key: "k" }, route: { from: "fake/model" }, tiers: TIERS }],
  ];
  for (const [name, raw] of invalid) {
    test(`rejects ${name}`, () => {
      const result = parseConfig(raw, () => "k");
      expect(result.ok).toBe(false);
    });
  }
});

describe("latestUserMessage", () => {
  test("returns the last user message and joins its text parts", () => {
    const messages = [
      user("u1", "first"),
      assistantToolRound("a1"),
      { role: "user", id: "u2", parts: [{ type: "text", id: "p1", text: "a" }, { type: "text", id: "p2", text: "b" }] },
      assistantToolRound("a2"),
    ];
    expect(latestUserMessage(messages)).toEqual({ id: "u2", text: "a\nb" });
  });

  test("is null without any user message", () => {
    expect(latestUserMessage([{ role: "system", id: "s", content: "x" }])).toBeNull();
  });
});

describe("Jev wire", () => {
  test("builds one choice question over tier names with a compact, truncated state", () => {
    const cfg = config();
    const p = params("s", "s", [user("u1", "x".repeat(10000))]);
    p.request.system = "y".repeat(5000);
    p.request.tools = [{ name: "read" }, { name: "edit" }];
    const body = buildJevRequest(cfg, p) as any;
    expect(body.model).toBe("jev-latest");
    expect(Object.keys(body.questions)).toEqual(["difficulty"]);
    expect(body.questions.difficulty.type).toBe("choice");
    expect(body.questions.difficulty.criteria).toEqual({
      easy: "Trivial lookups",
      medium: "Normal feature work",
      hard: "Cross-cutting design",
    });
    expect(body.state.latest_user_message.length).toBeLessThanOrEqual(4001);
    expect(body.state.system_prompt_head.length).toBeLessThanOrEqual(1001);
    expect(body.state.tool_count).toBe(2);
    expect(body.state.message_count).toBe(1);
    expect(body.state.agent).toBe("build");
  });

  test("parses a confident choice into a tier index", () => {
    const answer = {
      model: "jev-1.13.0",
      answers: { difficulty: { type: "choice", choice: "hard", probabilities: { hard: 0.9 }, confidence: 0.8 } },
    };
    expect(parseJevAnswer(answer, TIERS, 0.5)).toEqual({ tier: 2 });
  });

  test("rejects low confidence, unknown options and malformed bodies", () => {
    const low = { answers: { difficulty: { type: "choice", choice: "easy", confidence: 0.2 } } };
    expect("error" in parseJevAnswer(low, TIERS, 0.5)).toBe(true);
    const unknown = { answers: { difficulty: { type: "choice", choice: "epic", confidence: 0.9 } } };
    expect("error" in parseJevAnswer(unknown, TIERS, 0.5)).toBe(true);
    expect("error" in parseJevAnswer({}, TIERS, 0.5)).toBe(true);
    expect("error" in parseJevAnswer(null, TIERS, 0.5)).toBe(true);
  });

  test("jevClassifier posts with a bearer key and maps the answer", async () => {
    const seen: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = (async (url: string, init: RequestInit) => {
      seen.push({ url, init });
      return new Response(
        JSON.stringify({ answers: { difficulty: { type: "choice", choice: "easy", confidence: 0.9 } } }),
        { status: 200 },
      );
    }) as unknown as typeof fetch;
    const classify = jevClassifier(config(), fetchImpl, quiet);
    expect(await classify(params("s", "s", [user("u", "hi")]))).toBe(0);
    expect(seen[0].url).toBe("https://api.typesafe.ai/v1/systemone");
    expect((seen[0].init.headers as Record<string, string>).authorization).toBe("Bearer k");
  });

  test("jevClassifier fails open to null on HTTP errors, bad JSON and timeouts", async () => {
    const status500 = (async () => new Response("boom", { status: 500 })) as unknown as typeof fetch;
    expect(await jevClassifier(config(), status500, quiet)(params("s", "s", []))).toBeNull();
    const badJson = (async () => new Response("not json", { status: 200 })) as unknown as typeof fetch;
    expect(await jevClassifier(config(), badJson, quiet)(params("s", "s", []))).toBeNull();
    const hang = ((_url: string, init: RequestInit) =>
      new Promise((_resolve, reject) => {
        init.signal?.addEventListener("abort", () => reject(new Error("aborted")));
      })) as unknown as typeof fetch;
    const slow = config({ jev: { timeout_ms: 20 } });
    expect(await jevClassifier(slow, hang, quiet)(params("s", "s", []))).toBeNull();
  });
});

describe("Router", () => {
  test("routes the first call to the chosen tier and keeps the chain sticky", async () => {
    const { calls, classify } = scripted([2]);
    const router = new Router(config(), classify, quiet);
    const first = await router.route(params("root", "root", [user("u1", "design")]));
    expect(first.model).toBe("fake/tier-hard");
    const toolRound = await router.route(params("root", "root", [user("u1", "design"), assistantToolRound("a1")]));
    expect(toolRound.model).toBe("fake/tier-hard");
    const child = await router.route(params("child", "root", [user("c1", "subtask")]));
    expect(child.model).toBe("fake/tier-hard");
    const nextTurn = await router.route(params("root", "root", [user("u1", "design"), user("u2", "again")]));
    expect(nextTurn.model).toBe("fake/tier-hard");
    expect(calls.length).toBe(1);
  });

  test("preserves every other request field", async () => {
    const { classify } = scripted([0]);
    const router = new Router(config(), classify, quiet);
    const p = params("s", "s", [user("u1", "hi")]);
    p.request.temperature = 0.3;
    p.request.headers = { "x-a": "b" };
    const routed = await router.route(p);
    expect(routed).toEqual({ ...p.request, model: "fake/tier-easy" });
  });

  test("uses default_tier when Jev fails or is unsure", async () => {
    const { classify } = scripted([null]);
    const router = new Router(config(), classify, quiet);
    expect((await router.route(params("s", "s", [user("u1", "?")]))).model).toBe("fake/tier-medium");
  });

  test("only rewrites models listed in route.from", async () => {
    const { calls, classify } = scripted([0, 0]);
    const router = new Router(config({ route: { from: ["fake/model"] } }), classify, quiet);
    const pinned = params("s", "s", [user("u1", "hi")], "fake/cheap-pinned");
    expect(await router.route(pinned)).toEqual(pinned.request);
    expect(calls.length).toBe(0);
    expect((await router.route(params("s", "s", [user("u1", "hi")]))).model).toBe("fake/tier-easy");
  });

  test("session stickiness keys by session, none classifies every call", async () => {
    const bySession = scripted([0, 2]);
    const router = new Router(config({ route: { stickiness: "session" } }), bySession.classify, quiet);
    expect((await router.route(params("root", "root", [user("u1", "a")]))).model).toBe("fake/tier-easy");
    expect((await router.route(params("child", "root", [user("c1", "b")]))).model).toBe("fake/tier-hard");
    expect(bySession.calls.length).toBe(2);

    const none = scripted([0, 1]);
    const eager = new Router(config({ route: { stickiness: "none" } }), none.classify, quiet);
    await eager.route(params("s", "s", [user("u1", "a")]));
    expect((await eager.route(params("s", "s", [user("u1", "a")]))).model).toBe("fake/tier-medium");
    expect(none.calls.length).toBe(2);
  });

  test("escalate re-asks only on a new root user message and never moves down", async () => {
    const { calls, classify } = scripted([1, 0, 2]);
    const router = new Router(config({ route: { escalate: true } }), classify, quiet);
    const turn1 = [user("u1", "feature")];
    expect((await router.route(params("root", "root", turn1))).model).toBe("fake/tier-medium");
    // Tool-result rounds and subagents in the chain do not re-ask.
    await router.route(params("root", "root", [...turn1, assistantToolRound("a1")]));
    await router.route(params("child", "root", [user("c1", "sub")]));
    expect(calls.length).toBe(1);
    // A new user message re-asks; an easier verdict keeps the harder tier.
    const turn2 = [...turn1, user("u2", "typo")];
    expect((await router.route(params("root", "root", turn2))).model).toBe("fake/tier-medium");
    // A harder verdict moves the whole chain up.
    const turn3 = [...turn2, user("u3", "redesign")];
    expect((await router.route(params("root", "root", turn3))).model).toBe("fake/tier-hard");
    expect((await router.route(params("child", "root", [user("c1", "sub")]))).model).toBe("fake/tier-hard");
    expect(calls.length).toBe(3);
  });

  test("concurrent first calls for one key share a single classification", async () => {
    const { calls, classify } = scripted([2]);
    const router = new Router(config(), classify, quiet);
    const [a, b] = await Promise.all([
      router.route(params("root", "root", [user("u1", "x")])),
      router.route(params("child", "root", [user("c1", "y")])),
    ]);
    expect([a.model, b.model]).toEqual(["fake/tier-hard", "fake/tier-hard"]);
    expect(calls.length).toBe(1);
  });

  test("never throws: a failing classifier leaves the request unchanged", async () => {
    const router = new Router(config(), async () => {
      throw new Error("boom");
    }, quiet);
    const p = params("s", "s", [user("u1", "x")]);
    expect(await router.route(p)).toEqual(p.request);
  });

  test("an unconfigured router passes every request through", async () => {
    const router = new Router(null, async () => 2, quiet);
    const p = params("s", "s", [user("u1", "x")]);
    expect(await router.route(p)).toEqual(p.request);
  });
});

describe("LruMap", () => {
  test("evicts the least recently used key", () => {
    const lru = new LruMap<string, number>(2);
    lru.set("a", 1);
    lru.set("b", 2);
    lru.get("a");
    lru.set("c", 3);
    expect(lru.get("b")).toBeUndefined();
    expect(lru.get("a")).toBe(1);
    expect(lru.get("c")).toBe(3);
  });
});

describe("handleMessage", () => {
  const router = new Router(config(), async () => 0, quiet);

  test("initialize declares exactly the chat.params hook", async () => {
    const reply = await handleMessage({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} }, router);
    expect(reply).toEqual({
      jsonrpc: "2.0",
      id: 1,
      result: {
        protocol_version: 1,
        plugin: { id: "jev-model-router", version: expect.any(String), kind: "bun" },
        tools: [],
        hooks: [{ name: "chat.params", posture: "open" }],
        skills: [],
      },
    });
  });

  test("hook/chat.params continues with the routed request", async () => {
    const reply = await handleMessage(
      { jsonrpc: "2.0", id: 2, method: "hook/chat.params", params: params("s", "s", [user("u1", "x")]) },
      router,
    );
    expect(reply?.result).toEqual({
      outcome: "continue",
      request: { ...params("s", "s", [user("u1", "x")]).request, model: "fake/tier-easy" },
    });
  });

  test("replies {} to other requests and ignores notifications", async () => {
    expect((await handleMessage({ jsonrpc: "2.0", id: 3, method: "shutdown", params: {} }, router))?.result).toEqual({});
    expect(await handleMessage({ jsonrpc: "2.0", method: "event", params: {} }, router)).toBeNull();
  });
});
