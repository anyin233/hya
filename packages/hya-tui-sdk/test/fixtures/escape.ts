import { defineTuiExtension } from "../../src";

/** Looks for a way out of its realm: ambient runtime APIs, a host `Function` constructor, module loading. */
async function probe(): Promise<string> {
  const host = "Bun" + " " + "process";
  const ambient = ["Bun", "process", "require", "fetch", "setTimeout", "Deno"].filter((name) => name in globalThis);
  const escapes: string[] = [];
  const seen = new Set<unknown>();
  const tryFunction = (candidate: unknown, where: string) => {
    try {
      if (typeof candidate === "function" && (candidate as (body: string) => () => unknown)(`return typeof ${host.split(" ")[0]} + typeof ${host.split(" ")[1]}`)() !== "undefinedundefined") escapes.push(where);
    } catch { /* not a constructor */ }
  };
  const visit = (value: unknown, where: string, depth: number) => {
    if (value === null || (typeof value !== "object" && typeof value !== "function") || seen.has(value) || depth > 3) return;
    seen.add(value);
    try { tryFunction((value as { constructor?: { constructor?: unknown } }).constructor?.constructor, `${where}.constructor.constructor`); } catch { /* getter */ }
    try { visit(Object.getPrototypeOf(value), `${where}.__proto__`, depth + 1); } catch { /* proxy */ }
    let keys: (string | symbol)[] = [];
    try { keys = Reflect.ownKeys(value); } catch { /* proxy */ }
    for (const key of keys) {
      let next: unknown;
      try { next = (value as Record<string | symbol, unknown>)[key]; } catch { continue; }
      visit(next, `${where}.${String(key)}`, depth + 1);
    }
  };
  visit(globalThis, "globalThis", 0);
  // Hidden from the bundler, which refuses runtime imports at build time.
  let dynamicImport = "refused";
  try { await (0, eval)(`import("node" + ":fs")`); dynamicImport = "loaded"; } catch { /* no module loader */ }
  return JSON.stringify({ ambient, escapes, dynamicImport });
}

export default defineTuiExtension({
  activate(api) {
    api.registerPanel({ id: "probe", title: "Probe", render: probe });
  },
});
