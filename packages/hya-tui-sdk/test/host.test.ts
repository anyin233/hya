import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { join } from "node:path";
import { ExtensionBuildError, ExtensionVm, bundleExtension, type HostCall } from "../src/host";
import { VM_STOPPED_ERROR_CODE } from "../src/protocol";

const fixture = (name: string) => join(import.meta.dir, "fixtures", name);
let nextId = 0;
const request = async (vm: ExtensionVm, method: string, params: unknown) => {
  const id = ++nextId;
  return JSON.parse(await vm.handle(JSON.stringify({ jsonrpc: "2.0", id, method, params }), id)) as { id: number; result?: unknown; error?: { code: number; message: string } };
};
const render = (vm: ExtensionVm, id: string) => request(vm, "tui/render", { surface: { kind: "panel", id }, width: 40, height: 5 });

async function probeVm(hostCall?: HostCall): Promise<ExtensionVm> {
  const vm = await ExtensionVm.create(await bundleExtension(fixture("probe.ts")), { memoryBytes: 32 * 1024 * 1024, deadlineMs: 300, log: () => undefined, ...(hostCall ? { hostCall } : {}) });
  expect((await request(vm, "tui/initialize", { api_version: 1, permissions: ["fs.read"] })).result).toEqual({ api_version: 1, sdk_version: "1.0.0" });
  await request(vm, "tui/activate", { context: { terminal: { columns: 80, rows: 24 } } });
  return vm;
}

describe("extension VM", () => {
  let vm: ExtensionVm;
  beforeAll(async () => { vm = await probeVm(); });
  afterAll(() => vm.dispose());

  test("bundles relative imports and has no ambient runtime APIs", async () => {
    expect((await render(vm, "probe")).result).toEqual({ root: { kind: "text", text: "helper-ok process:undefined require:undefined fetch:undefined Bun:undefined setTimeout:undefined WebAssembly:undefined XMLHttpRequest:undefined Deno:undefined" } });
  });

  test("a runaway handler is stopped at the deadline; the VM then refuses every request", async () => {
    const runaway = await probeVm();
    const started = Date.now();
    expect((await render(runaway, "loop")).error).toEqual({ code: VM_STOPPED_ERROR_CODE, message: "extension exceeded its 300 ms limit" });
    expect(Date.now() - started).toBeLessThan(2_000);
    expect((await render(runaway, "probe")).error?.code).toBe(VM_STOPPED_ERROR_CODE);
    runaway.dispose();
  });

  test("memory is capped by the VM's WebAssembly memory; the host process does not grow past it", async () => {
    const before = process.memoryUsage().rss;
    expect((await render(vm, "hog")).error?.message).toBe("out of memory");
    expect(process.memoryUsage().rss - before).toBeLessThan(64 * 1024 * 1024);
    expect((await render(vm, "probe")).result).toBeDefined();
  });

  test("a promise that can never settle is answered at once with an error", async () => {
    expect((await render(vm, "pending")).error?.message).toContain("never settles");
  });
});

test("api.fs resolves through the host; waiting for it does not count against the deadline", async () => {
  const calls: string[] = [];
  // The host answers after longer than the VM's 300 ms deadline: only VM execution is limited.
  const vm = await probeVm(async (method, params) => {
    calls.push(`${method} ${String(params.path)}`);
    await Bun.sleep(400);
    return { text: "file body" };
  });
  try {
    expect((await render(vm, "read")).result).toEqual({ root: { kind: "text", text: "read:file body" } });
    expect(calls).toEqual(["fs/read notes.md"]);
  } finally {
    vm.dispose();
  }
});

test("a host error rejects inside the VM", async () => {
  const vm = await probeVm(async () => { throw new Error("outside the Project roots"); });
  try {
    expect((await render(vm, "read")).result).toEqual({ root: { kind: "text", text: "error:outside the Project roots" } });
  } finally {
    vm.dispose();
  }
});

test("importing runtime modules or packages fails the bundle with an explanation", async () => {
  const failed = await bundleExtension(fixture("node-import.ts")).then(() => undefined, (error: unknown) => error);
  expect(failed).toBeInstanceOf(ExtensionBuildError);
  expect(String(failed)).toContain('cannot import "node:fs"');
});
