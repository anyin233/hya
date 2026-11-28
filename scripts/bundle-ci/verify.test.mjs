import { describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseYaml, splitFrontmatter } from "./yaml.mjs";
import {
  discoverBundles,
  extractCargoDependencies,
  formatGithubOutputs,
  runPackDeterminismCheck,
  runProtocolSmoke,
  validateBundleStatic,
  verifyBundle,
} from "./lib.mjs";

function withTempDir(fn) {
  const dir = mkdtempSync(join(tmpdir(), "hya-bundle-ci-test-"));
  try {
    return fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

async function withTempDirAsync(fn) {
  const dir = mkdtempSync(join(tmpdir(), "hya-bundle-ci-test-"));
  try {
    return await fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function writeFiles(root, files) {
  for (const [rel, content] of Object.entries(files)) {
    const full = join(root, rel);
    mkdirSync(join(full, ".."), { recursive: true });
    writeFileSync(full, content, "utf8");
  }
}

describe("yaml fallback parser", () => {
  test("parses nested mappings, sequences, flow collections, quotes, and comments", () => {
    const sample = `
# top comment
kind: Bundle
version_ref: hya
identity:
  id: acme/demo
  version: 0.0.0
  publisher: acme
check: { command: [bun, test, "--pass-with-no-tests"], timeout_secs: 120 }
extensions:
  process:
    kind: bun
    command: [bun, run, '\${BUNDLE_ROOT}/index.ts']
    timeout_ms: 30000
  files:
    - { id: main, path: index.ts }
    - id: schema
      path: schemas/out.json
agents:
  - id: scout
    role: subagent
    private: true
    description: 'Cheap scout: answers "where is X" # not a comment'
    resource_view:
      allow:
        - harness:tool/read
        - zvec-grep
`;
    const parsed = parseYaml(sample, { forceJs: true });
    expect(parsed.kind).toBe("Bundle");
    expect(parsed.identity).toEqual({ id: "acme/demo", version: "0.0.0", publisher: "acme" });
    expect(parsed.check).toEqual({ command: ["bun", "test", "--pass-with-no-tests"], timeout_secs: 120 });
    expect(parsed.extensions.process).toEqual({
      kind: "bun",
      command: ["bun", "run", "${BUNDLE_ROOT}/index.ts"],
      timeout_ms: 30000,
    });
    expect(parsed.extensions.files).toEqual([
      { id: "main", path: "index.ts" },
      { id: "schema", path: "schemas/out.json" },
    ]);
    expect(parsed.agents[0].id).toBe("scout");
    expect(parsed.agents[0].private).toBe(true);
    expect(parsed.agents[0].description).toBe('Cheap scout: answers "where is X" # not a comment');
    expect(parsed.agents[0].resource_view.allow).toEqual(["harness:tool/read", "zvec-grep"]);
  });

  test("splits markdown frontmatter cleanly", () => {
    const split = splitFrontmatter("---\nid: scout\nrole: subagent\n---\nYou are a scout.\n");
    expect(split).toEqual({
      frontmatter: "id: scout\nrole: subagent",
      body: "You are a scout.\n",
    });
  });
});

describe("discoverBundles", () => {
  test("classifies native, js, tui, and static bundles and filters by changed files", () => {
    withTempDir((root) => {
      writeFiles(root, {
        "bundles/b-native/bundle.yaml":
          "kind: Bundle\nidentity: { id: acme/b-native, version: 1.0.0, publisher: acme }\n",
        "bundles/b-native/native/Cargo.toml": '[package]\nname = "acme-b-native"\nversion = "0.1.0"\n',
        "bundles/b-native/native/src/lib.rs": "pub extern \"C\" fn hya_tool_bundle_register_rewriters_v1() {}\n",
        "bundles/b-js/bundle.yaml":
          "kind: Bundle\nidentity: { id: acme/b-js, version: 1.0.0, publisher: acme }\nruntime:\n  kind: js\n  mode: process\n  command: [bun, run, '${BUNDLE_ROOT}/index.ts']\ncheck:\n  command: [bun, test]\n",
        "bundles/b-tui/bundle.yaml":
          "kind: Bundle\nidentity: { id: acme/b-tui, version: 1.0.0, publisher: acme }\ntui:\n  api_version: 1\n  entry: tui/main.ts\n  sdk: '1.0'\n",
        "bundles/b-static/bundle.yaml":
          "kind: Bundle\nidentity: { id: acme/b-static, version: 1.0.0, publisher: acme }\n",
      });

      const all = discoverBundles(["bundles"], { cwd: root });
      expect(all.all.map((b) => [b.name, b.type])).toEqual([
        ["b-js", "js"],
        ["b-native", "native"],
        ["b-static", "static"],
        ["b-tui", "tui"],
      ]);
      expect(all.native).toHaveLength(1);
      expect(all.native[0].crate_name).toBe("acme-b-native");
      expect(all.native[0].lib_stem).toBe("acme_b_native");
      expect(all.native[0].has_rewriters).toBe(true);
      expect(all.js_static).toHaveLength(3);
      expect(formatGithubOutputs(all)).toContain("has_native=true");

      const filtered = discoverBundles(["bundles"], {
        cwd: root,
        changedFiles: ["bundles/b-js/index.ts", "README.md"],
      });
      expect(filtered.all.map((b) => b.name)).toEqual(["b-js"]);
      expect(filtered.has_native).toBe(false);
      expect(filtered.has_js_static).toBe(true);
    });
  });
});

describe("validateBundleStatic", () => {
  test("accepts standardized v2 bundle with tools, hooks, mcp, skills, commands, standalone agent", () => {
    withTempDir((dir) => {
      writeFiles(dir, {
        "bundle.yaml": `kind: Bundle
schema_version: 2
identity:
  id: acme/full-bundle
  version: 1.2.3
  publisher: acme
namespace: full-bundle
runtime:
  kind: static
resources:
  tools:
    - id: lookup
      path: tools/lookup.json
  hooks:
    - id: tool.execute.before
      path: hooks/hooks.json
  mcp:
    - id: remote-docs
      path: mcp/remote-docs.json
  skills:
    - id: guide
      path: skills/guide/SKILL.md
  commands:
    - id: review
      path: commands/review.md
agents:
  - path: agents/helper.md
workflow:
  id: sample
  path: workflows/sample.hya.md
`,
        "tools/lookup.json": JSON.stringify({
          schema_version: 1,
          name: "lookup",
          permission: "read_only",
          parameters: { type: "object", properties: {} },
        }),
        "hooks/hooks.json": JSON.stringify({
          schema_version: 1,
          hooks: [
            {
              event: "tool.execute.before",
              handler: { kind: "rpc", method: "hook/tool.execute.before" },
            },
          ],
        }),
        "mcp/remote-docs.json": JSON.stringify({
          transport: "http",
          url: "https://mcp.example.com/v1",
          headers: { Authorization: "Bearer ${env:EXAMPLE_API_TOKEN}" },
          timeout_ms: 30000,
        }),
        "skills/guide/SKILL.md": "---\nname: guide\ndescription: Guide skill\n---\nInstructions.\n",
        "commands/review.md": "---\nname: review\ndescription: Review command\n---\nReview $ARGUMENTS.\n",
        "agents/helper.md": "---\nid: helper\nrole: subagent\ndescription: Helper\nhook_refs: [tool.execute.before]\n---\nBe helpful.\n",
        "workflows/sample.hya.md": "---\nname: sample\n---\nStage 1.\n",
      });

      const res = validateBundleStatic(dir);
      expect(res.errors).toEqual([]);
      expect(res.ok).toBe(true);
    });
  });

  test("rejects reserved namespace, unknown top-level key, and bad version_ref", () => {
    withTempDir((dir) => {
      writeFiles(dir, {
        "bundle.yaml": `kind: Bundle
schema_version: 2
version_ref: hya
identity:
  id: acme/bad
  version: 1.0.0
  publisher: acme
namespace: mcp
unknown_field: 123
`,
      });
      const res = validateBundleStatic(dir);
      expect(res.ok).toBe(false);
      expect(res.errors.some((e) => e.includes("unknown top-level key"))).toBe(true);
      expect(res.errors.some((e) => e.includes("version_ref requires identity.version"))).toBe(true);
      expect(res.errors.some((e) => e.includes("invalid or reserved namespace"))).toBe(true);
    });
  });

  test("rejects v1 schema_version, legacy extensions, exposure.yaml, and reserved kernel tools todo/rewrite", () => {
    withTempDir((dir) => {
      writeFiles(dir, {
        "bundle.yaml": `kind: Bundle
schema_version: 1
identity:
  id: acme/legacy
  version: 1.0.0
  publisher: acme
extensions:
  files:
    - id: exposure
      path: exposure.yaml
resources:
  tools:
    - id: todo
      path: tools/todo.json
    - id: custom
      aliases: [rewrite]
      path: tools/custom.json
`,
        "exposure.yaml": "tools: []\n",
        "tools/todo.json": JSON.stringify({ name: "todo", permission: "tool" }),
        "tools/custom.json": JSON.stringify({ name: "custom", aliases: ["rewrite"], permission: "tool" }),
      });
      const res = validateBundleStatic(dir);
      expect(res.ok).toBe(false);
      expect(res.errors.some((e) => e.includes("schema_version must be 2"))).toBe(true);
      expect(res.errors.some((e) => e.includes("legacy `extensions` was removed"))).toBe(true);
      expect(res.errors.some((e) => e.includes("exposure.yaml"))).toBe(true);
      expect(res.errors.some((e) => e.includes("`todo` is permanently reserved"))).toBe(true);
      expect(res.errors.some((e) => e.includes("`rewrite` is permanently reserved"))).toBe(true);
    });
  });

  test("enforces ABI v2 red-line crate dependencies and rejects ABI v1 symbols", () => {
    expect(
      extractCargoDependencies(`
[package]
name = "demo"
[dependencies]
hya-runtime-abi = { workspace = true }
hya-store = { workspace = true }
db = { package = "sqlx", version = "0.8" }
[target.'cfg(unix)'.dependencies]
axum = "0.7"
[dev-dependencies]
hya-engine = { workspace = true }
`),
    ).toEqual(["hya-runtime-abi", "hya-store", "sqlx", "axum"]);

    withTempDir((dir) => {
      writeFiles(dir, {
        "bundle.yaml": `kind: Bundle
schema_version: 2
identity:
  id: acme/bad-native
  version: 1.0.0
  publisher: acme
runtime:
  kind: native
  abi_version: 2
  crate_dir: native
  capabilities: [tools]
`,
        "native/Cargo.toml": `[package]
name = "acme-bad-native"
version = "0.0.0"
[dependencies]
hya-runtime-abi = { workspace = true }
hya-engine = { workspace = true }
tonic = "0.13"
`,
        "native/src/lib.rs": `pub extern "C" fn hya_tool_bundle_abi_v1() -> u32 { 1 }\n`,
      });
      const res = validateBundleStatic(dir);
      expect(res.ok).toBe(false);
      expect(res.errors.some((e) => e.includes("forbidden native cdylib dependency `hya-engine`"))).toBe(true);
      expect(res.errors.some((e) => e.includes("forbidden native cdylib dependency `tonic`"))).toBe(true);
      expect(res.errors.some((e) => e.includes("removed ABI v1 symbol `hya_tool_bundle_abi_v1`"))).toBe(true);
    });
  });

  test("rejects MCP stdio+http conflict and hardcoded Authorization secret", () => {
    withTempDir((dir) => {
      writeFiles(dir, {
        "bundle.yaml": `kind: Bundle
schema_version: 2
identity:
  id: acme/mcp-bad
  version: 1.0.0
  publisher: acme
resources:
  mcp:
    - id: both
      path: mcp/both.json
    - id: leaky
      path: mcp/leaky.json
`,
        "mcp/both.json": JSON.stringify({
          command: ["node", "server.js"],
          url: "https://mcp.example.com",
        }),
        "mcp/leaky.json": JSON.stringify({
          transport: "http",
          url: "https://mcp.example.com",
          headers: { Authorization: "Bearer super-secret-literal-token" },
        }),
      });
      const res = validateBundleStatic(dir);
      expect(res.ok).toBe(false);
      expect(res.errors.some((e) => e.includes("mutually exclusive"))).toBe(true);
      expect(res.errors.some((e) => e.includes("hardcoded credential"))).toBe(true);
    });
  });

  test("rejects symlinks and missing referenced files", () => {
    withTempDir((dir) => {
      writeFiles(dir, {
        "bundle.yaml": `kind: Bundle
schema_version: 2
identity:
  id: acme/sym
  version: 1.0.0
  publisher: acme
agents:
  - id: a
    role: subagent
    prompt: prompts/missing.md
`,
        "real.txt": "hello\n",
      });
      symlinkSync(join(dir, "real.txt"), join(dir, "link.txt"));
      const res = validateBundleStatic(dir);
      expect(res.ok).toBe(false);
      expect(res.errors.some((e) => e.includes("symlink forbidden"))).toBe(true);
      expect(res.errors.some((e) => e.includes("missing file: prompts/missing.md"))).toBe(true);
    });
  });
});

describe("protocol smoke, check.command, and pack determinism", () => {
  test("verifies a compliant JS process bundle end-to-end (check + protocol smoke)", async () => {
    await withTempDirAsync(async (dir) => {
      writeFiles(dir, {
        "bundle.yaml": `kind: Bundle
schema_version: 2
identity:
  id: acme/echo-plugin
  version: 1.0.0
  publisher: acme
check:
  command: [bun, test]
  timeout_secs: 30
runtime:
  kind: js
  mode: process
  entry: plugin.ts
  command: [bun, run, '\${BUNDLE_ROOT}/plugin.ts']
assets:
  - id: plugin
    path: plugin.ts
resources:
  tools:
    - id: ping
      path: plugin.ts
  hooks:
    - id: chat.params
      path: plugin.ts
apis:
  - id: status
    method: GET
    scope: session
    path: /status
`,
        "plugin.ts": `import { createInterface } from "node:readline";
export const greet = () => "ok";
if (import.meta.main) {
  const rl = createInterface({ input: process.stdin, terminal: false });
  for await (const line of rl) {
    if (!line.trim()) continue;
    const msg = JSON.parse(line);
    if (msg.method === "initialize") {
      process.stdout.write(JSON.stringify({
        jsonrpc: "2.0",
        id: msg.id,
        result: {
          protocol_version: 1,
          plugin: { id: "echo-plugin", version: "1.0.0", kind: "bun" },
          tools: [{ name: "ping", description: "ping", inputSchema: { type: "object" } }],
          hooks: [{ name: "chat.params" }],
          apis: [{ name: "status" }],
          skills: [],
        },
      }) + "\\n");
    } else if (msg.method === "shutdown") {
      process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: {} }) + "\\n", () => process.exit(0));
    }
  }
}
`,
        "plugin.test.ts": `import { expect, test } from "bun:test";
import { greet } from "./plugin.ts";
test("greet returns ok", () => expect(greet()).toBe("ok"));
`,
      });

      const report = await verifyBundle(dir);
      expect(report.ok).toBe(true);
      expect(report.check.ok).toBe(true);
      expect(report.protocol.ok).toBe(true);
      expect(report.protocol.tools).toEqual(["ping"]);
      expect(report.protocol.hooks).toEqual(["chat.params"]);
      expect(report.protocol.apis).toEqual(["status"]);
    });
  });

  test("fails protocol smoke when reported tools mismatch bundle.yaml", async () => {
    await withTempDirAsync(async (dir) => {
      writeFiles(dir, {
        "bundle.yaml": `kind: Bundle
schema_version: 2
identity:
  id: acme/mismatch
  version: 1.0.0
  publisher: acme
runtime:
  kind: js
  mode: process
  entry: plugin.ts
  command: [bun, run, '\${BUNDLE_ROOT}/plugin.ts']
assets:
  - id: plugin
    path: plugin.ts
resources:
  tools:
    - id: expected_tool
      path: plugin.ts
`,
        "plugin.ts": `import { createInterface } from "node:readline";
const rl = createInterface({ input: process.stdin, terminal: false });
for await (const line of rl) {
  const msg = JSON.parse(line);
  if (msg.method === "initialize") {
    process.stdout.write(JSON.stringify({
      jsonrpc: "2.0",
      id: msg.id,
      result: {
        protocol_version: 1,
        plugin: { id: "mismatch", version: "1.0.0", kind: "bun" },
        tools: [{ name: "wrong_tool" }],
        hooks: [],
      },
    }) + "\\n");
  } else if (msg.method === "shutdown") {
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: {} }) + "\\n", () => process.exit(0));
  }
}
`,
      });
      const res = await runProtocolSmoke(dir, validateBundleStatic(dir).manifest);
      expect(res.ok).toBe(false);
      expect(res.error).toContain("initialize tools mismatch");
    });
  });

  test("verifies deterministic pack output and rejects non-deterministic packer", async () => {
    await withTempDirAsync(async (dir) => {
      writeFiles(dir, {
        "bundle/bundle.yaml":
          "kind: Bundle\nidentity: { id: acme/static, version: 1.0.0, publisher: acme }\n",
        "det-packer.mjs": `import { writeFileSync } from "node:fs";
const out = process.argv[3];
writeFileSync(out, Buffer.concat([Buffer.from([0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]), Buffer.from("deterministic")]));
`,
        "nondet-packer.mjs": `import { writeFileSync } from "node:fs";
const out = process.argv[3];
writeFileSync(out, Buffer.concat([Buffer.from([0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]), Buffer.from(String(Math.random()))]));
`,
        "fake-verify.mjs": `process.exit(0);\n`,
      });

      const okRes = await runPackDeterminismCheck(join(dir, "bundle"), {
        packCmd: `bun ${join(dir, "det-packer.mjs")} {source} {output}`,
        verifyCmd: `bun ${join(dir, "fake-verify.mjs")} {output}`,
      });
      expect(okRes.ok).toBe(true);
      expect(okRes.verified_archive).toBe(true);
      expect(okRes.sha256).toHaveLength(64);

      const badRes = await runPackDeterminismCheck(join(dir, "bundle"), {
        packCmd: `bun ${join(dir, "nondet-packer.mjs")} {source} {output}`,
      });
      expect(badRes.ok).toBe(false);
      expect(badRes.error).toContain("non-deterministic pack output");
    });
  });

  test("verify.mjs CLI supports --root <dir> --strict and positional bundle dirs", () => {
    withTempDir((dir) => {
      writeFiles(dir, {
        "bundles/ok-static/bundle.yaml": `kind: Bundle
schema_version: 2
identity:
  id: acme/ok-static
  version: 1.0.0
  publisher: acme
runtime:
  kind: static
`,
        "bundles/ok-native/bundle.yaml": `kind: Bundle
schema_version: 2
identity:
  id: acme/ok-native
  version: 1.0.0
  publisher: acme
runtime:
  kind: native
  abi_version: 2
  crate_dir: native
  capabilities: [tools]
`,
        "bundles/ok-native/native/Cargo.toml": `[package]
name = "acme-ok-native"
version = "0.0.0"
[dependencies]
hya-runtime-abi = { workspace = true }
`,
        "bundles/ok-native/native/src/lib.rs": `hya_runtime_abi::export_native_bundle_v2!(|_out| {});\n`,
      });
      const verifyScript = join(import.meta.dir, "verify.mjs");
      const rootRun = spawnSync(
        "node",
        [verifyScript, "--root", join(dir, "bundles"), "--strict", "--json"],
        { encoding: "utf8" },
      );
      expect(rootRun.status).toBe(0);
      const parsed = JSON.parse(rootRun.stdout);
      expect(parsed.ok).toBe(true);
      expect(parsed.reports).toHaveLength(2);
      expect(parsed.reports.find((r) => r.id === "acme/ok-native").native.ok).toBe(true);

      const posRun = spawnSync(
        "node",
        [verifyScript, "--strict", join(dir, "bundles/ok-static")],
        { encoding: "utf8" },
      );
      expect(posRun.status).toBe(0);
      expect(posRun.stdout).toContain("[PASS]");
    });
  });
});
