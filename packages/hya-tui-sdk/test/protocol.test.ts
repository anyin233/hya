import { describe, expect, test } from "bun:test";
import { encodeFrame, parseFrame, TUI_EXTENSION_API_VERSION, TUI_EXTENSION_SDK_VERSION } from "../src/index";

describe("TUI extension protocol", () => {
  test("exposes independent protocol versions", () => {
    expect(TUI_EXTENSION_API_VERSION).toBe(1);
    expect(TUI_EXTENSION_SDK_VERSION).toBe("1.0.0");
  });

  test("round-trips typed requests as one JSON line", () => {
    const frame = {
      jsonrpc: "2.0" as const,
      id: 7,
      method: "tui/render" as const,
      params: { surface: { kind: "panel" as const, id: "status" }, width: 80, height: 24 },
    };
    expect(encodeFrame(frame)).toBe(`${JSON.stringify(frame)}\n`);
    expect(parseFrame(JSON.stringify(frame))).toEqual({ ok: true, frame });
  });

  test("accepts error responses and rejects malformed envelopes", () => {
    expect(parseFrame('{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"bad"}}').ok).toBe(true);
    expect(parseFrame('{"jsonrpc":"1.0","id":1,"result":null}').ok).toBe(false);
    expect(parseFrame('{"jsonrpc":"2.0","id":1,"method":"tui/render"}').ok).toBe(false);
  });

  test("keeps render tree declarative", () => {
    const frame = {
      jsonrpc: "2.0" as const,
      id: "r1",
      result: { root: { kind: "column" as const, children: [{ kind: "text" as const, text: "hello" }] } },
    };
    expect(parseFrame(encodeFrame(frame).trim())).toEqual({ ok: true, frame });
  });
});
