import { describe, expect, test } from "bun:test";
import { dispatch, initialization } from "../backend/provider";

const request = (method: string, params: unknown = {}) => ({ jsonrpc: "2.0", id: "request-1", method, params });

describe("disk inspector provider", () => {
  test("initializes the bundle's exact API set and rejects an incompatible protocol", () => {
    expect(dispatch(request("initialize", { protocol_version: 1 })).frame).toEqual({ jsonrpc: "2.0", id: "request-1", result: initialization });
    expect(dispatch(request("initialize", { protocol_version: 2 })).frame).toMatchObject({ error: { code: -32602 } });
    expect(initialization.apis.map((api) => api.name)).toEqual(["info"]);
  });
  test("discovery identifies the provider machine without claiming a scanner", () => {
    expect(dispatch(request("api/request", { api: "info", method: "GET", path: "/info" })).frame).toMatchObject({
      result: { status: 200, body: { contractVersion: 1, machine: { platform: process.platform }, capabilities: { scans: false, cancel: false, volumes: false } } },
    });
  });
  test("does not serve a declared endpoint with the wrong method or path", () => {
    for (const params of [null, { api: "info", method: "POST", path: "/info" }, { api: "info", method: "GET", path: "/scans" }]) {
      expect(dispatch(request("api/request", params)).frame).toMatchObject({ result: { status: 404 } });
    }
  });
  test("notifications have no replies, unknown methods have errors, and shutdown terminates", () => {
    expect(dispatch({ jsonrpc: "2.0", method: "event", params: {} })).toEqual({ shutdown: false });
    expect(dispatch(request("missing")).frame).toMatchObject({ error: { code: -32601 } });
    expect(dispatch(request("shutdown"))).toMatchObject({ shutdown: true, frame: { result: {} } });
    expect(dispatch(null).frame).toMatchObject({ error: { code: -32600 } });
  });
  test("stdio recovers after malformed JSON and flushes replies before shutdown", async () => {
    const child = Bun.spawn([process.execPath, "run", new URL("../backend/main.ts", import.meta.url).pathname], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
    const output = new Response(child.stdout).text();
    child.stdin.write(`invalid\n${JSON.stringify(request("initialize", { protocol_version: 1 }))}\n${JSON.stringify(request("api/request", { api: "info", method: "GET", path: "/info" }))}\n${JSON.stringify(request("shutdown"))}\n`);
    child.stdin.end();
    const frames = (await output).trim().split("\n").map((line) => JSON.parse(line));
    expect(await child.exited).toBe(0);
    expect(frames).toHaveLength(4);
    expect(frames[0]).toMatchObject({ error: { code: -32700 } });
    expect(frames[2]).toMatchObject({ result: { status: 200 } });
    expect(frames[3]).toMatchObject({ result: {} });
    expect(await new Response(child.stderr).text()).toBe("");
  });
});
