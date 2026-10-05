import { defineTuiExtension } from "@hya/tui-sdk";
import { label } from "./helper";
const g = globalThis as Record<string, unknown>;
export default defineTuiExtension({
  activate(api) {
    api.registerPanel({ id: "probe", title: "Probe", render: () => [label, ...["process", "require", "fetch", "Bun", "setTimeout", "WebAssembly", "XMLHttpRequest", "Deno"].map((name) => `${name}:${typeof g[name]}`)].join(" ") });
    api.registerPanel({ id: "loop", title: "Loop", render: () => { for (;;) { /* runaway */ } } });
    api.registerPanel({ id: "hog", title: "Hog", render: () => { const kept: number[][] = []; for (;;) kept.push(new Array(100_000).fill(1)); } });
    api.registerPanel({ id: "pending", title: "Pending", render: () => new Promise(() => {}) });
    api.registerPanel({ id: "read", title: "Read", render: () => api.fs.read("notes.md").then((text) => `read:${text}`, (error: Error) => `error:${error.message}`) });
  },
});
