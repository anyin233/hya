import { describe, expect, test } from "bun:test";
import {
  BUNDLE_API_METHODS, PANE_SLOTS, TUI_EXTENSION_API_VERSION, TUI_EXTENSION_PERMISSIONS,
  TUI_EXTENSION_SDK_MAJOR, TUI_EXTENSION_SDK_VERSION,
  buildBundleApiUrlPath, checkBundleConfinement, encodeFrame, isTranscriptNode,
  parseFrame, transcriptNode, validateBundleApiPath, validateTranscriptNode,
} from "../src/index";

describe("TUI extension protocol v2", () => {
  test("exposes independent v2 protocol versions and permissions", () => {
    expect(TUI_EXTENSION_API_VERSION).toBe(2);
    expect(TUI_EXTENSION_SDK_MAJOR).toBe(2);
    expect(TUI_EXTENSION_SDK_VERSION).toBe("2.1.0");
    expect(TUI_EXTENSION_PERMISSIONS).toContain("tui.bundle_api");
    expect(TUI_EXTENSION_PERMISSIONS).toContain("tui.extension_state.read");
    expect(TUI_EXTENSION_PERMISSIONS).toContain("tui.transcript.embed");
    expect(BUNDLE_API_METHODS).toEqual(["GET", "POST", "PUT", "PATCH", "DELETE"]);
    expect(PANE_SLOTS).toEqual(["left", "right", "top", "bottom", "main", "overlay"]);
  });

  test("round-trips typed v2 requests including pane surfaces and tui/command", () => {
    const renderFrame = {
      jsonrpc: "2.0" as const,
      id: 7,
      method: "tui/render" as const,
      params: { surface: { kind: "pane" as const, id: "subagent-viewer", instance: "hysec_child_1" }, width: 80, height: 24, now: 1000 },
    };
    expect(encodeFrame(renderFrame)).toBe(`${JSON.stringify(renderFrame)}\n`);
    expect(parseFrame(JSON.stringify(renderFrame))).toEqual({ ok: true, frame: renderFrame });

    const commandFrame = {
      jsonrpc: "2.0" as const,
      id: 8,
      method: "tui/command" as const,
      params: { name: "workflows", args: "active", raw: "/workflows active" },
    };
    expect(parseFrame(encodeFrame(commandFrame).trim())).toEqual({ ok: true, frame: commandFrame });
  });

  test("accepts error responses and rejects malformed envelopes", () => {
    expect(parseFrame('{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"bad"}}').ok).toBe(true);
    expect(parseFrame('{"jsonrpc":"1.0","id":1,"result":null}').ok).toBe(false);
    expect(parseFrame('{"jsonrpc":"2.0","id":1,"method":"tui/render"}').ok).toBe(false);
  });

  test("keeps render tree declarative including host-virtualized transcript nodes", () => {
    const node = transcriptNode("hysec_sub_42", { follow: true, maxMessages: 100, compact: true });
    expect(node).toEqual({ type: "transcript", sessionId: "hysec_sub_42", follow: true, maxMessages: 100, compact: true });
    expect(isTranscriptNode(node)).toBe(true);
    expect(isTranscriptNode({ kind: "text", text: "plain" })).toBe(false);

    const frame = {
      jsonrpc: "2.0" as const,
      id: "r1",
      result: { root: { kind: "column" as const, children: [{ kind: "text" as const, text: "hello" }, node] } },
    };
    const parsed = parseFrame(encodeFrame(frame).trim());
    expect(parsed.ok).toBe(true);
    if (parsed.ok) expect(parsed.frame).toEqual(frame as unknown as typeof parsed.frame);
  });

  test("enforces strict bundle-ID confinement for bundle_api and extension_state", () => {
    expect(checkBundleConfinement("hya/workflow", undefined, "bundle_api")).toEqual({ ok: true, bundleId: "hya/workflow" });
    expect(checkBundleConfinement("hya/workflow", "hya/workflow", "bundle_api")).toEqual({ ok: true, bundleId: "hya/workflow" });

    const deniedApi = checkBundleConfinement("hya/workflow", "hya/multi-agent", "bundle_api");
    expect(deniedApi.ok).toBe(false);
    if (!deniedApi.ok) expect(deniedApi.error).toContain("confined to bundle hya/workflow");

    const deniedState = checkBundleConfinement("hya/workflow", "hya/multi-agent", "extension_state");
    expect(deniedState.ok).toBe(false);
    if (!deniedState.ok) expect(deniedState.error).toContain("confined to bundle hya/workflow");

    expect(checkBundleConfinement("hya/workflow", "hya/multi-agent", "bundle_api", ["hya/multi-agent"])).toEqual({
      ok: true,
      bundleId: "hya/multi-agent",
    });
    expect(checkBundleConfinement("hya/workflow", "hya/multi-agent", "extension_state", (target) => target === "hya/multi-agent")).toEqual({
      ok: true,
      bundleId: "hya/multi-agent",
    });
    expect(checkBundleConfinement("", "hya/workflow", "bundle_api").ok).toBe(false);
    expect(checkBundleConfinement("hya/workflow", "../escape", "bundle_api").ok).toBe(false);
  });

  test("validates bundle.api paths and builds confined global and session-scoped URL paths", () => {
    expect(validateBundleApiPath("/runs/42")).toEqual({ ok: true, path: "/runs/42" });
    expect(validateBundleApiPath("/")).toEqual({ ok: true, path: "/" });
    expect(validateBundleApiPath("runs")).toMatchObject({ ok: false });
    expect(validateBundleApiPath("/../other/api/secret")).toMatchObject({ ok: false });
    expect(validateBundleApiPath("/runs/%2e%2e/secret")).toMatchObject({ ok: false });
    expect(validateBundleApiPath("/runs/%2fsecret")).toMatchObject({ ok: false });
    expect(validateBundleApiPath("/runs?x=1")).toMatchObject({ ok: false });
    expect(validateBundleApiPath("/runs#frag")).toMatchObject({ ok: false });
    expect(validateBundleApiPath("/runs\\evil")).toMatchObject({ ok: false });
    expect(validateBundleApiPath("/runs//double")).toMatchObject({ ok: false });
    expect(validateBundleApiPath("/runs/%ZZ")).toMatchObject({ ok: false });

    expect(buildBundleApiUrlPath("hya/workflow", "/runs", { query: { status: "active", limit: "10" } })).toEqual({
      ok: true,
      normalizedPath: "/runs",
      urlPath: "/v2/bundles/hya%2Fworkflow/api/runs?status=active&limit=10",
    });
    expect(buildBundleApiUrlPath("hya/multi-agent", "/members/1", { sessionId: "hysec_root_1" })).toEqual({
      ok: true,
      normalizedPath: "/members/1",
      urlPath: "/v2/sessions/hysec_root_1/bundles/hya%2Fmulti-agent/members/1",
    });
    expect(buildBundleApiUrlPath("hya/multi-agent", "/members", { sessionId: "../bad" }).ok).toBe(false);
  });

  test("validates host-virtualized transcript render nodes and permission gating", () => {
    expect(validateTranscriptNode({ type: "transcript", sessionId: "hysec_1" }, { allowed: false }))
      .toContain("requires permission tui.transcript.embed");
    expect(validateTranscriptNode({ type: "transcript", sessionId: "hysec_1", follow: false, scrollOffset: 12, maxMessages: 50, compact: true }, { allowed: true }))
      .toEqual({ type: "transcript", sessionId: "hysec_1", follow: false, scrollOffset: 12, maxMessages: 50, compact: true });
    expect(validateTranscriptNode({ kind: "transcript", sessionId: "hysec_2" }, { allowed: true }))
      .toEqual({ type: "transcript", sessionId: "hysec_2" });
    expect(validateTranscriptNode({ type: "transcript", sessionId: "../bad" }, { allowed: true }))
      .toContain("valid session id");
    expect(validateTranscriptNode({ type: "transcript", sessionId: "hysec_1", scrollOffset: -1 }, { allowed: true }))
      .toContain("non-negative integer");
    expect(validateTranscriptNode({ type: "transcript", sessionId: "hysec_1", maxMessages: 0 }, { allowed: true }))
      .toContain("1..1000");
    expect(validateTranscriptNode({ type: "transcript", sessionId: "hysec_1", extra: true }, { allowed: true }))
      .toContain("unknown field");
  });
});
