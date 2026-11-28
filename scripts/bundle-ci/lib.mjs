// Portable, zero-dependency Hya bundle discovery, static validation, self-check,
// black-box JSON-RPC 2.0 protocol smoke, and deterministic packaging verifier.
// Designed to run identically in this monorepo and in a standalone Marketplace repo.

import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import {
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, posix, relative, resolve, sep } from "node:path";
import { parseYaml, splitFrontmatter } from "./yaml.mjs";

export const CANONICAL_HOOKS = new Set([
  "event",
  "tool.execute.before",
  "tool.execute.after",
  "tool.visibility",
  "command.execute.before",
  "experimental.text.complete",
  "message.user.before",
  "chat.params",
  "permission.ask",
  "permission.approve",
  "goal.evaluate",
  "loop.verifier",
  "loop.planner",
  "loop.should_stop",
  "compaction.before",
  "compaction.after",
  "session.start",
  "session.end",
  "agent.spawn",
  "model.fallback",
]);

export const RESERVED_NAMESPACES = new Set(["mcp", "harness", "builtin", "plugin"]);

export const ALLOWED_TOP_LEVEL_KEYS = new Set([
  "kind",
  "schema_version",
  "version_ref",
  "identity",
  "namespace",
  "overrides",
  "runtime",
  "check",
  "schemas",
  "resources",
  "extensions",
  "assets",
  "apis",
  "permission_modes",
  "tui",
  "agents",
  "channels",
  "workflow",
]);

export const ALLOWED_MCP_KEYS = new Set([
  "command",
  "env",
  "url",
  "headers",
  "transport",
  "enabled",
  "timeout_ms",
]);

export const ALLOWED_API_METHODS = new Set(["GET", "POST", "PUT", "PATCH", "DELETE"]);
export const ALLOWED_API_SCOPES = new Set(["session", "global"]);
export const ALLOWED_TOOL_PERMISSIONS = new Set(["read_only", "tool", "command", "task", "interactive"]);
export const ALLOWED_HOOK_HANDLER_KINDS = new Set(["js", "rpc", "command", "native"]);
export const ALLOWED_PROCESS_KINDS = new Set(["rust", "bun", "claude"]);
export const ALLOWED_RUNTIME_KINDS = new Set(["static", "js", "native"]);
export const ALLOWED_JS_MODES = new Set(["managed", "process", "claude"]);
export const ALLOWED_NATIVE_CAPABILITIES = new Set(["tools", "hooks", "rewriters", "services", "drivers", "schemas", "apis"]);

const MAX_SKILL_BYTES = 256 * 1024;
const MAX_PACKAGE_BYTES = 128 * 1024 * 1024;
const PUBLIC_7Z_MAGIC = Buffer.from([0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]);

const SECRET_PATTERNS = [
  { label: "OpenAI/Anthropic API key", regex: /\bsk-(?:proj-|ant-api\d+-)?[A-Za-z0-9_-]{20,}\b/ },
  { label: "GitHub classic token", regex: /\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}\b/ },
  { label: "GitHub fine-grained token", regex: /\bgithub_pat_[A-Za-z0-9_]{20,}\b/ },
  { label: "AWS access key", regex: /\bAKIA[0-9A-Z]{16}\b/ },
  { label: "Google API key", regex: /\bAIza[0-9A-Za-z_-]{30,}\b/ },
  { label: "Slack token", regex: /\bxox[baprs]-[0-9A-Za-z-]{10,}\b/ },
  { label: "Private key block", regex: /-----BEGIN [A-Z ]*PRIVATE KEY-----/ },
];

const PLACEHOLDER_REGEX = /\$\{([^}]+)\}/g;
const ALLOWED_PLACEHOLDER = /^(?:BUNDLE_ROOT|BUNDLE_CONFIG_FILE|BUNDLE_CONFIG_DIR|env:[A-Za-z_][A-Za-z0-9_]*)$/;
const SENSITIVE_KEY_REGEX = /(?:api[_-]?key|secret|token|password|passwd|credential|authorization|auth[_-]?header)/i;

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function toPosix(filePath) {
  return filePath.split(sep).join("/");
}

export function normalizeSafeRelativePath(rawPath) {
  if (typeof rawPath !== "string" || rawPath.trim() === "") {
    return { ok: false, error: "path must be a non-empty string" };
  }
  if (rawPath.startsWith("/") || rawPath.includes("\\")) {
    return { ok: false, error: `path must be relative with '/' separators: ${rawPath}` };
  }
  const parts = [];
  for (const segment of rawPath.split("/")) {
    if (segment === "" || segment === ".") continue;
    if (segment === "..") {
      return { ok: false, error: `path traversal ('..') is forbidden: ${rawPath}` };
    }
    parts.push(segment);
  }
  if (parts.length === 0) {
    return { ok: false, error: `path resolves to empty relative path: ${rawPath}` };
  }
  return { ok: true, path: parts.join("/") };
}

function scanTree(rootDir, currentDir = rootDir, entries = [], errors = []) {
  for (const dirent of readdirSync(currentDir, { withFileTypes: true })) {
    const full = join(currentDir, dirent.name);
    const rel = toPosix(relative(rootDir, full));
    const stat = lstatSync(full);
    if (stat.isSymbolicLink()) {
      errors.push(`symlink forbidden in bundle source: ${rel}`);
      continue;
    }
    if (stat.isDirectory()) {
      scanTree(rootDir, full, entries, errors);
    } else if (stat.isFile()) {
      entries.push({ rel, full, size: stat.size });
    } else {
      errors.push(`special non-regular file forbidden in bundle source: ${rel}`);
    }
  }
  return { entries, errors };
}

export function readBundleManifest(bundleDir) {
  const yamlPath = join(bundleDir, "bundle.yaml");
  const mdPath = join(bundleDir, "bundle.hya.md");
  const hasYaml = existsSync(yamlPath);
  const hasMd = existsSync(mdPath);
  if (hasYaml && hasMd) {
    throw new Error("bundle contains both bundle.yaml and bundle.hya.md (mutually exclusive)");
  }
  if (!hasYaml && !hasMd) {
    throw new Error("bundle must contain either bundle.yaml or bundle.hya.md");
  }
  if (hasYaml) {
    const rawText = readFileSync(yamlPath, "utf8");
    const manifest = parseYaml(rawText);
    return { manifest, manifestFile: "bundle.yaml", rawText, markdownBody: null };
  }
  const rawText = readFileSync(mdPath, "utf8");
  const split = splitFrontmatter(rawText);
  if (!split) {
    throw new Error("bundle.hya.md must begin with YAML frontmatter delimited by ---");
  }
  const manifest = parseYaml(split.frontmatter);
  return {
    manifest,
    manifestFile: "bundle.hya.md",
    rawText,
    markdownBody: split.body.trim() || null,
  };
}

function readNativeCrateMeta(bundleDir, manifest) {
  const crateDir = manifest?.runtime?.crate_dir || "native";
  const cargoToml = join(bundleDir, crateDir, "Cargo.toml");
  if (!existsSync(cargoToml)) return null;
  const text = readFileSync(cargoToml, "utf8");
  const match = text.match(/^\s*name\s*=\s*"([^"]+)"/m);
  const crateName = match ? match[1] : null;
  const libStem = crateName ? crateName.replace(/-/g, "_") : null;
  const libRs = join(bundleDir, crateDir, "src", "lib.rs");
  const libRsText = existsSync(libRs) ? readFileSync(libRs, "utf8") : "";
  const caps = Array.isArray(manifest?.runtime?.capabilities) ? manifest.runtime.capabilities : [];
  const hasRewriters =
    caps.includes("rewriters") ||
    libRsText.includes("hya_tool_bundle_register_rewriters_v1") ||
    libRsText.includes("export_native_rewriter_bundle");
  const abiVersion =
    typeof manifest?.runtime?.abi_version === "number"
      ? manifest.runtime.abi_version
      : libRsText.includes("export_native_bundle_v2") ||
          libRsText.includes("hya_native_register_v2")
        ? 2
        : 1;
  return {
    crate_dir: crateDir,
    crate_name: crateName,
    lib_stem: libStem,
    abi_version: abiVersion,
    has_exposure: existsSync(join(bundleDir, "exposure.yaml")),
    has_rewriters: hasRewriters,
  };
}

export function extractProcessSpec(manifest) {
  if (isRecord(manifest?.extensions?.process)) {
    const proc = manifest.extensions.process;
    if (Array.isArray(proc.command) && proc.command.length > 0) {
      return {
        kind: proc.kind,
        command: proc.command,
        timeout_ms: proc.timeout_ms,
        source: "extensions.process",
      };
    }
  }
  if (isRecord(manifest?.runtime) && manifest.runtime.kind === "js") {
    const rt = manifest.runtime;
    if ((rt.mode === "process" || Array.isArray(rt.command)) && Array.isArray(rt.command) && rt.command.length > 0) {
      return {
        kind: "bun",
        command: rt.command,
        timeout_ms: rt.timeout_ms,
        source: "runtime",
      };
    }
  }
  return null;
}

export function classifyBundle(bundleDir, manifest) {
  const nativeMeta = readNativeCrateMeta(bundleDir, manifest);
  const hasNative =
    manifest?.runtime?.kind === "native" ||
    (Array.isArray(manifest?.extensions?.libraries) && manifest.extensions.libraries.length > 0) ||
    (Array.isArray(manifest?.extensions?.rust) && manifest.extensions.rust.length > 0) ||
    nativeMeta !== null;

  if (hasNative) return { type: "native", nativeMeta };
  if (isRecord(manifest?.tui)) return { type: "tui", nativeMeta: null };

  const hasJs =
    manifest?.runtime?.kind === "js" ||
    manifest?.extensions?.process?.kind === "bun" ||
    manifest?.extensions?.process?.kind === "claude" ||
    (Array.isArray(manifest?.extensions?.js) && manifest.extensions.js.length > 0);
  if (hasJs) return { type: "js", nativeMeta: null };

  return { type: "static", nativeMeta: null };
}

export function discoverBundles(rootDirs, options = {}) {
  const cwd = options.cwd ? resolve(options.cwd) : process.cwd();
  const changedFiles = options.changedFiles
    ? options.changedFiles.map((f) => toPosix(f).replace(/^\.\//, ""))
    : null;
  const candidates = [];

  for (const rawRoot of rootDirs) {
    const absRoot = resolve(cwd, rawRoot);
    if (!existsSync(absRoot)) {
      if (options.allowEmpty) continue;
      throw new Error(`bundle root directory does not exist: ${rawRoot}`);
    }
    const stat = lstatSync(absRoot);
    if (!stat.isDirectory()) continue;

    if (existsSync(join(absRoot, "bundle.yaml")) || existsSync(join(absRoot, "bundle.hya.md"))) {
      candidates.push(absRoot);
      continue;
    }

    for (const dirent of readdirSync(absRoot, { withFileTypes: true })) {
      if (!dirent.isDirectory()) continue;
      const sub = join(absRoot, dirent.name);
      if (existsSync(join(sub, "bundle.yaml")) || existsSync(join(sub, "bundle.hya.md"))) {
        candidates.push(sub);
      }
    }
  }

  candidates.sort((a, b) => toPosix(relative(cwd, a)).localeCompare(toPosix(relative(cwd, b))));

  const discovered = [];
  for (const absDir of candidates) {
    const relPath = toPosix(relative(cwd, absDir));
    if (changedFiles !== null) {
      const prefix = relPath.endsWith("/") ? relPath : `${relPath}/`;
      const hit = changedFiles.some((file) => file === relPath || file.startsWith(prefix));
      if (!hit) continue;
    }
    const { manifest } = readBundleManifest(absDir);
    const { type, nativeMeta } = classifyBundle(absDir, manifest);
    const id = manifest?.identity?.id ?? basename(absDir);
    const namespace = manifest?.namespace || (typeof id === "string" ? id.split("/").pop() : basename(absDir));
    const processSpec = extractProcessSpec(manifest);
    const entry = {
      name: basename(absDir),
      path: relPath,
      id,
      namespace,
      type,
      group: type === "native" ? "native" : "js_static",
      has_check: Boolean(Array.isArray(manifest?.check?.command) && manifest.check.command.length > 0),
      has_process: Boolean(processSpec),
    };
    if (nativeMeta) {
      entry.crate_dir = nativeMeta.crate_dir;
      entry.crate_name = nativeMeta.crate_name;
      entry.lib_stem = nativeMeta.lib_stem;
      entry.abi_version = nativeMeta.abi_version;
      entry.has_exposure = nativeMeta.has_exposure;
      entry.has_rewriters = nativeMeta.has_rewriters;
    }
    discovered.push(entry);
  }

  const native = discovered.filter((b) => b.type === "native");
  const js_static = discovered.filter((b) => b.type !== "native");
  return {
    all: discovered,
    native,
    js_static,
    by_type: {
      native,
      js: discovered.filter((b) => b.type === "js"),
      tui: discovered.filter((b) => b.type === "tui"),
      static: discovered.filter((b) => b.type === "static"),
    },
    has_all: discovered.length > 0,
    has_native: native.length > 0,
    has_js_static: js_static.length > 0,
  };
}

function scanTextForSecrets(text, location, errors) {
  for (const { label, regex } of SECRET_PATTERNS) {
    if (regex.test(text)) {
      errors.push(`${location}: potential hardcoded secret detected (${label})`);
    }
  }
}

function validatePlaceholders(value, location, errors) {
  for (const match of value.matchAll(PLACEHOLDER_REGEX)) {
    const full = match[0];
    const inner = match[1];
    if (!ALLOWED_PLACEHOLDER.test(inner)) {
      errors.push(
        `${location}: unsupported placeholder \`${full}\` (allowed: \${BUNDLE_ROOT}, \${BUNDLE_CONFIG_FILE}, \${BUNDLE_CONFIG_DIR}, \${env:VAR})`,
      );
    }
  }
}

function checkSensitiveLiteral(key, value, location, errors) {
  if (typeof value !== "string") return;
  scanTextForSecrets(value, location, errors);
  validatePlaceholders(value, location, errors);

  if (SENSITIVE_KEY_REGEX.test(key)) {
    const trimmed = value.trim();
    if (trimmed === "") return;
    // Allow values that delegate secrets to `${env:...}` or `${BUNDLE_CONFIG_...}`.
    const stripped = trimmed
      .replace(/^(?:Bearer|Basic|Token)\s+/i, "")
      .trim();
    if (!/\$\{(?:env:[A-Za-z_][A-Za-z0-9_]*|BUNDLE_CONFIG_FILE|BUNDLE_CONFIG_DIR)\}/.test(stripped)) {
      errors.push(
        `${location}: hardcoded credential for \`${key}\` is forbidden; use \${env:VAR_NAME} instead`,
      );
    }
  }
}

function validateResourceList(entries, kind, fileSet, errors) {
  if (entries === undefined || entries === null) return [];
  if (!Array.isArray(entries)) {
    errors.push(`resources.${kind} must be a list`);
    return [];
  }
  const seenIds = new Set();
  const seenAliases = new Set();
  const validated = [];
  for (let i = 0; i < entries.length; i++) {
    const item = entries[i];
    const where = `resources.${kind}[${i}]`;
    if (!isRecord(item)) {
      errors.push(`${where} must be a mapping`);
      continue;
    }
    if (typeof item.id !== "string" || item.id.trim() === "") {
      errors.push(`${where}.id must be a non-empty string`);
      continue;
    }
    if (seenIds.has(item.id) || seenAliases.has(item.id)) {
      errors.push(`${where}.id \`${item.id}\` collides with an earlier id or alias`);
    }
    seenIds.add(item.id);
    if (item.aliases !== undefined) {
      if (!Array.isArray(item.aliases) || !item.aliases.every((a) => typeof a === "string" && a.trim() !== "")) {
        errors.push(`${where}.aliases must be a list of non-empty strings`);
      } else {
        for (const alias of item.aliases) {
          if (seenIds.has(alias) || seenAliases.has(alias)) {
            errors.push(`${where}.aliases entry \`${alias}\` collides with an existing id or alias`);
          }
          seenAliases.add(alias);
        }
      }
    }
    const norm = normalizeSafeRelativePath(item.path);
    if (!norm.ok) {
      errors.push(`${where}.path: ${norm.error}`);
      continue;
    }
    if (!fileSet.has(norm.path)) {
      errors.push(`${where}.path references missing file: ${norm.path}`);
      continue;
    }
    validated.push({ id: item.id, path: norm.path });
  }
  return validated;
}

export function validateBundleStatic(bundleDir) {
  const absDir = resolve(bundleDir);
  const errors = [];
  const warnings = [];

  if (!existsSync(absDir) || !lstatSync(absDir).isDirectory()) {
    return { ok: false, errors: [`bundle directory does not exist: ${bundleDir}`], warnings, manifest: null };
  }

  const { entries, errors: treeErrors } = scanTree(absDir);
  errors.push(...treeErrors);
  const fileMap = new Map(entries.map((entry) => [entry.rel, entry]));

  let loaded;
  try {
    loaded = readBundleManifest(absDir);
  } catch (error) {
    return {
      ok: false,
      errors: [...errors, error instanceof Error ? error.message : String(error)],
      warnings,
      manifest: null,
    };
  }

  const { manifest, manifestFile, rawText, markdownBody } = loaded;
  if (!isRecord(manifest)) {
    return { ok: false, errors: [...errors, `${manifestFile} must be a YAML mapping`], warnings, manifest: null };
  }

  scanTextForSecrets(rawText, manifestFile, errors);

  // Unknown top-level keys (strict deny_unknown_fields parity).
  for (const key of Object.keys(manifest)) {
    if (!ALLOWED_TOP_LEVEL_KEYS.has(key)) {
      errors.push(`${manifestFile}: unknown top-level key \`${key}\``);
    }
  }

  if (manifest.kind !== "Bundle") {
    errors.push(`${manifestFile}: kind must be \`Bundle\` (got ${JSON.stringify(manifest.kind)})`);
  }

  if (manifest.schema_version !== undefined && manifest.schema_version !== 1 && manifest.schema_version !== 2) {
    errors.push(`${manifestFile}: schema_version must be 1 or 2`);
  }

  // Identity & version_ref
  if (!isRecord(manifest.identity)) {
    errors.push(`${manifestFile}: identity must be a mapping`);
  } else {
    const { id, version, publisher } = manifest.identity;
    if (typeof id !== "string" || !/^[A-Za-z0-9._-]+\/[A-Za-z0-9._-]+$/.test(id)) {
      errors.push(`${manifestFile}: identity.id must be <publisher>/<name> (got ${JSON.stringify(id)})`);
    }
    if (typeof publisher !== "string" || publisher.trim() === "") {
      errors.push(`${manifestFile}: identity.publisher must be a non-empty string`);
    }
    if (typeof version !== "string" || version.trim() === "") {
      errors.push(`${manifestFile}: identity.version must be a non-empty string`);
    } else if (manifest.version_ref !== undefined) {
      if (manifest.version_ref !== "hya" && manifest.version_ref !== "backend") {
        errors.push(`${manifestFile}: version_ref must be \`hya\` or \`backend\``);
      }
      if (version !== "0.0.0") {
        errors.push(`${manifestFile}: version_ref requires identity.version to be \`0.0.0\``);
      }
    } else if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(version)) {
      errors.push(`${manifestFile}: identity.version must be valid SemVer (got ${JSON.stringify(version)})`);
    }
  }

  // Namespace
  const effectiveNamespace =
    typeof manifest.namespace === "string" && manifest.namespace !== ""
      ? manifest.namespace
      : typeof manifest.identity?.id === "string"
        ? manifest.identity.id.split("/").pop()
        : "";
  if (
    !effectiveNamespace ||
    effectiveNamespace.includes("__") ||
    !/^[A-Za-z0-9_-]+$/.test(effectiveNamespace) ||
    RESERVED_NAMESPACES.has(effectiveNamespace)
  ) {
    errors.push(
      `${manifestFile}: invalid or reserved namespace \`${effectiveNamespace}\` (must match [A-Za-z0-9_-]+, contain no '__', and not be one of mcp/harness/builtin/plugin)`,
    );
  }

  // Runtime vs legacy extensions conflict & shape checks
  const hasRuntime = manifest.runtime !== undefined && manifest.runtime !== null;
  const extensions = isRecord(manifest.extensions) ? manifest.extensions : {};
  if (manifest.extensions !== undefined && !isRecord(manifest.extensions)) {
    errors.push(`${manifestFile}: extensions must be a mapping`);
  }
  if (hasRuntime) {
    if (!isRecord(manifest.runtime)) {
      errors.push(`${manifestFile}: runtime must be a mapping`);
    } else {
      const rt = manifest.runtime;
      if (!ALLOWED_RUNTIME_KINDS.has(rt.kind)) {
        errors.push(`${manifestFile}: runtime.kind must be static, js, or native`);
      }
      if (extensions.process !== undefined || (Array.isArray(extensions.libraries) && extensions.libraries.length > 0)) {
        errors.push(`${manifestFile}: cannot declare both \`runtime\` and legacy \`extensions.process\`/\`extensions.libraries\``);
      }
      if (rt.kind === "js") {
        if (rt.mode !== undefined && !ALLOWED_JS_MODES.has(rt.mode)) {
          errors.push(`${manifestFile}: runtime.mode must be managed, process, or claude`);
        }
        if (rt.entry !== undefined) {
          const norm = normalizeSafeRelativePath(rt.entry);
          if (!norm.ok) errors.push(`${manifestFile}: runtime.entry: ${norm.error}`);
          else if (!fileMap.has(norm.path)) errors.push(`${manifestFile}: runtime.entry references missing file: ${norm.path}`);
        }
        if (rt.command !== undefined) {
          if (!Array.isArray(rt.command) || rt.command.length === 0 || rt.command.some((a) => typeof a !== "string" || a.trim() === "")) {
            errors.push(`${manifestFile}: runtime.command must be a non-empty list of non-blank strings`);
          }
        }
        if (rt.timeout_ms !== undefined) {
          if (!Number.isInteger(rt.timeout_ms) || rt.timeout_ms < 1000 || rt.timeout_ms > 3_600_000) {
            errors.push(`${manifestFile}: runtime.timeout_ms must be an integer in 1000..=3600000`);
          }
        }
      } else if (rt.kind === "native") {
        if (rt.abi_version !== undefined && rt.abi_version !== 1 && rt.abi_version !== 2) {
          errors.push(`${manifestFile}: runtime.abi_version must be 1 or 2`);
        }
        if (rt.crate_dir !== undefined) {
          const norm = normalizeSafeRelativePath(rt.crate_dir);
          if (!norm.ok) errors.push(`${manifestFile}: runtime.crate_dir: ${norm.error}`);
          else if (!fileMap.has(`${norm.path}/Cargo.toml`)) {
            errors.push(`${manifestFile}: runtime.crate_dir missing ${norm.path}/Cargo.toml`);
          }
        }
        if (rt.library !== undefined) {
          const norm = normalizeSafeRelativePath(rt.library);
          if (!norm.ok) errors.push(`${manifestFile}: runtime.library: ${norm.error}`);
          else if (!fileMap.has(norm.path)) errors.push(`${manifestFile}: runtime.library references missing file: ${norm.path}`);
        }
        if (rt.capabilities !== undefined) {
          if (!Array.isArray(rt.capabilities) || !rt.capabilities.every((c) => ALLOWED_NATIVE_CAPABILITIES.has(c))) {
            errors.push(`${manifestFile}: runtime.capabilities entries must be tools, hooks, rewriters, services, drivers, schemas, or apis`);
          }
        }
      }
    }
  }

  if (extensions.process !== undefined) {
    const proc = extensions.process;
    if (!isRecord(proc)) {
      errors.push(`${manifestFile}: extensions.process must be a mapping`);
    } else {
      if (!ALLOWED_PROCESS_KINDS.has(proc.kind)) {
        errors.push(`${manifestFile}: extensions.process.kind must be rust, bun, or claude`);
      }
      if (!Array.isArray(proc.command) || proc.command.length === 0 || proc.command.some((a) => typeof a !== "string" || a.trim() === "")) {
        errors.push(`${manifestFile}: extensions.process.command must be a non-empty list of non-blank strings`);
      }
      if (proc.timeout_ms !== undefined && (!Number.isInteger(proc.timeout_ms) || proc.timeout_ms < 1000 || proc.timeout_ms > 3_600_000)) {
        errors.push(`${manifestFile}: extensions.process.timeout_ms must be an integer in 1000..=3600000`);
      }
    }
  }

  for (const extGroup of ["js", "rust", "libraries", "files"]) {
    if (extensions[extGroup] !== undefined) {
      validateResourceList(extensions[extGroup], `extensions.${extGroup}`, fileMap, errors);
    }
  }
  if (manifest.assets !== undefined) {
    validateResourceList(manifest.assets, "assets", fileMap, errors);
  }

  // Self-check validation
  if (manifest.check !== undefined && manifest.check !== null) {
    if (!isRecord(manifest.check)) {
      errors.push(`${manifestFile}: check must be a mapping`);
    } else {
      const cmd = manifest.check.command;
      if (!Array.isArray(cmd) || cmd.length === 0 || cmd.some((a) => typeof a !== "string" || a.trim() === "")) {
        errors.push(`${manifestFile}: check.command must be a non-empty list of non-blank strings`);
      }
      const timeout = manifest.check.timeout_secs ?? 120;
      if (!Number.isInteger(timeout) || timeout < 1 || timeout > 600) {
        errors.push(`${manifestFile}: check.timeout_secs must be an integer in 1..=600`);
      }
    }
  }

  // Resources
  const resources = isRecord(manifest.resources) ? manifest.resources : {};
  if (manifest.resources !== undefined && !isRecord(manifest.resources)) {
    errors.push(`${manifestFile}: resources must be a mapping`);
  }
  for (const key of Object.keys(resources)) {
    if (!["tools", "hooks", "mcp", "skills", "commands"].includes(key)) {
      errors.push(`${manifestFile}: unknown resources key \`${key}\``);
    }
  }

  const tools = validateResourceList(resources.tools, "tools", fileMap, errors);
  const hooks = validateResourceList(resources.hooks, "hooks", fileMap, errors);
  const mcpServers = validateResourceList(resources.mcp, "mcp", fileMap, errors);
  const skills = validateResourceList(resources.skills, "skills", fileMap, errors);
  const commands = validateResourceList(resources.commands, "commands", fileMap, errors);

  // Validate tool descriptors (when JSON/YAML)
  for (const tool of tools) {
    if (/\.(?:json|ya?ml)$/i.test(tool.path)) {
      const raw = readFileSync(fileMap.get(tool.path).full, "utf8");
      scanTextForSecrets(raw, tool.path, errors);
      let parsed;
      try {
        parsed = tool.path.endsWith(".json") ? JSON.parse(raw) : parseYaml(raw);
      } catch (err) {
        errors.push(`${tool.path}: invalid descriptor: ${err instanceof Error ? err.message : String(err)}`);
        continue;
      }
      if (!isRecord(parsed)) {
        errors.push(`${tool.path}: tool descriptor must be an object`);
        continue;
      }
      if (Object.keys(parsed).length > 0) {
        if (parsed.permission !== undefined && !ALLOWED_TOOL_PERMISSIONS.has(parsed.permission)) {
          errors.push(`${tool.path}: invalid tool permission \`${parsed.permission}\``);
        }
        if (parsed.parameters !== undefined && !isRecord(parsed.parameters)) {
          errors.push(`${tool.path}: tool parameters must be a JSON Schema object`);
        }
      }
    }
  }

  // Validate hook ids & structured hook descriptors
  const declaredHookIds = new Set();
  for (const hook of hooks) {
    declaredHookIds.add(hook.id);
    if (!CANONICAL_HOOKS.has(hook.id)) {
      errors.push(`${manifestFile}: unsupported hook id \`${hook.id}\``);
    }
    if (/\.(?:json|ya?ml)$/i.test(hook.path)) {
      const raw = readFileSync(fileMap.get(hook.path).full, "utf8");
      scanTextForSecrets(raw, hook.path, errors);
      let parsed;
      try {
        parsed = hook.path.endsWith(".json") ? JSON.parse(raw) : parseYaml(raw);
      } catch (err) {
        errors.push(`${hook.path}: invalid hook descriptor: ${err instanceof Error ? err.message : String(err)}`);
        continue;
      }
      if (!isRecord(parsed)) {
        errors.push(`${hook.path}: hook descriptor must be an object`);
        continue;
      }
      if (parsed.hooks !== undefined) {
        if (!Array.isArray(parsed.hooks)) {
          errors.push(`${hook.path}: hooks field must be an array`);
        } else {
          for (let i = 0; i < parsed.hooks.length; i++) {
            const entry = parsed.hooks[i];
            if (!isRecord(entry)) {
              errors.push(`${hook.path}: hooks[${i}] must be an object`);
              continue;
            }
            if (typeof entry.event !== "string" || !CANONICAL_HOOKS.has(entry.event)) {
              errors.push(`${hook.path}: hooks[${i}].event \`${String(entry.event)}\` is not a canonical hook point`);
            }
            if (entry.handler !== undefined) {
              if (!isRecord(entry.handler) || !ALLOWED_HOOK_HANDLER_KINDS.has(entry.handler.kind)) {
                errors.push(`${hook.path}: hooks[${i}].handler.kind must be js, rpc, command, or native`);
              }
            }
          }
        }
      }
    }
  }

  // Validate MCP descriptors: stdio vs http mutual exclusion + secret hygiene
  for (const mcp of mcpServers) {
    const raw = readFileSync(fileMap.get(mcp.path).full, "utf8");
    let parsed;
    try {
      parsed = JSON.parse(raw);
    } catch (err) {
      errors.push(`${mcp.path}: MCP descriptor must be valid JSON: ${err instanceof Error ? err.message : String(err)}`);
      continue;
    }
    if (!isRecord(parsed)) {
      errors.push(`${mcp.path}: MCP descriptor must be a JSON object`);
      continue;
    }
    for (const key of Object.keys(parsed)) {
      if (!ALLOWED_MCP_KEYS.has(key)) {
        errors.push(`${mcp.path}: unknown MCP field \`${key}\``);
      }
    }
    const hasCommand = Array.isArray(parsed.command) && parsed.command.length > 0;
    const hasUrl = typeof parsed.url === "string" && parsed.url.trim() !== "";
    if (hasCommand && hasUrl) {
      errors.push(`${mcp.path}: \`command\` (stdio) and \`url\` (http/sse) are mutually exclusive`);
    } else if (!hasCommand && !hasUrl) {
      errors.push(`${mcp.path}: must declare either a non-empty \`command\` argv or a non-empty \`url\``);
    }
    if (parsed.command !== undefined) {
      if (!Array.isArray(parsed.command) || parsed.command.length === 0 || parsed.command.some((a) => typeof a !== "string" || a.trim() === "")) {
        errors.push(`${mcp.path}: command must be a non-empty array of non-blank strings`);
      } else {
        for (const arg of parsed.command) {
          scanTextForSecrets(arg, `${mcp.path}:command`, errors);
          validatePlaceholders(arg, `${mcp.path}:command`, errors);
        }
      }
    }
    if (hasUrl) {
      if (!/^https?:\/\//i.test(parsed.url)) {
        errors.push(`${mcp.path}: url must start with http:// or https://`);
      }
      scanTextForSecrets(parsed.url, `${mcp.path}:url`, errors);
      validatePlaceholders(parsed.url, `${mcp.path}:url`, errors);
    }
    if (parsed.transport !== undefined) {
      if (!["stdio", "http", "sse", "streamable_http"].includes(parsed.transport)) {
        errors.push(`${mcp.path}: unsupported transport \`${parsed.transport}\``);
      } else if (parsed.transport === "stdio" && !hasCommand) {
        errors.push(`${mcp.path}: transport \`stdio\` requires \`command\``);
      } else if (parsed.transport !== "stdio" && !hasUrl) {
        errors.push(`${mcp.path}: transport \`${parsed.transport}\` requires \`url\``);
      }
    }
    if (parsed.env !== undefined) {
      if (!isRecord(parsed.env)) {
        errors.push(`${mcp.path}: env must be an object mapping strings to strings`);
      } else {
        for (const [k, v] of Object.entries(parsed.env)) {
          if (typeof v !== "string") errors.push(`${mcp.path}: env.${k} must be a string`);
          else checkSensitiveLiteral(k, v, `${mcp.path}:env.${k}`, errors);
        }
      }
    }
    if (parsed.headers !== undefined) {
      if (!isRecord(parsed.headers)) {
        errors.push(`${mcp.path}: headers must be an object mapping strings to strings`);
      } else {
        for (const [k, v] of Object.entries(parsed.headers)) {
          if (typeof v !== "string") errors.push(`${mcp.path}: headers.${k} must be a string`);
          else checkSensitiveLiteral(k, v, `${mcp.path}:headers.${k}`, errors);
        }
      }
    }
  }

  // Validate Skills (UTF-8 & <= 256 KiB) and Commands
  for (const skill of skills) {
    const entry = fileMap.get(skill.path);
    if (entry.size > MAX_SKILL_BYTES) {
      errors.push(`${skill.path}: skill exceeds 256 KiB limit (${entry.size} bytes)`);
    }
  }
  for (const command of commands) {
    readFileSync(fileMap.get(command.path).full, "utf8");
  }

  // Validate Agents (inline and standalone agents/*.md frontmatter)
  if (manifest.agents !== undefined) {
    if (!Array.isArray(manifest.agents)) {
      errors.push(`${manifestFile}: agents must be a list`);
    } else {
      const seenAgents = new Set();
      for (let i = 0; i < manifest.agents.length; i++) {
        let agent = manifest.agents[i];
        const where = `agents[${i}]`;
        if (!isRecord(agent)) {
          errors.push(`${where} must be a mapping`);
          continue;
        }
        if (typeof agent.path === "string" && agent.id === undefined) {
          const norm = normalizeSafeRelativePath(agent.path);
          if (!norm.ok) {
            errors.push(`${where}.path: ${norm.error}`);
            continue;
          }
          if (!fileMap.has(norm.path)) {
            errors.push(`${where}.path references missing file: ${norm.path}`);
            continue;
          }
          const mdText = readFileSync(fileMap.get(norm.path).full, "utf8");
          const split = splitFrontmatter(mdText);
          if (!split) {
            errors.push(`${norm.path}: standalone agent markdown must include YAML frontmatter`);
            continue;
          }
          agent = parseYaml(split.frontmatter);
          if (!isRecord(agent)) {
            errors.push(`${norm.path}: agent frontmatter must be a YAML mapping`);
            continue;
          }
        }
        if (typeof agent.id !== "string" || agent.id.trim() === "") {
          errors.push(`${where}.id must be a non-empty string`);
        } else if (seenAgents.has(agent.id)) {
          errors.push(`${where}.id duplicate agent id \`${agent.id}\``);
        } else {
          seenAgents.add(agent.id);
        }
        if (agent.role !== "main" && agent.role !== "subagent") {
          errors.push(`${where}.role must be \`main\` or \`subagent\` (got ${JSON.stringify(agent.role)})`);
        }
        if (agent.prompt !== undefined && agent.prompt !== null) {
          const norm = normalizeSafeRelativePath(agent.prompt);
          if (!norm.ok) errors.push(`${where}.prompt: ${norm.error}`);
          else if (!fileMap.has(norm.path)) errors.push(`${where}.prompt references missing file: ${norm.path}`);
        }
        if (Array.isArray(agent.hook_refs)) {
          for (const ref of agent.hook_refs) {
            if (typeof ref !== "string") {
              errors.push(`${where}.hook_refs entries must be strings`);
              continue;
            }
            const localName = ref.includes("/hook/") ? ref.split("/hook/").pop() : ref;
            if (!declaredHookIds.has(localName)) {
              errors.push(`${where}.hook_refs references undeclared hook \`${ref}\``);
            }
          }
        }
      }
      if (markdownBody !== null && (manifest.agents.length !== 1 || manifest.agents[0]?.prompt)) {
        errors.push(`${manifestFile}: Markdown body requires exactly one agent without an explicit prompt`);
      }
    }
  }

  // Validate Workflow
  if (manifest.workflow !== undefined && manifest.workflow !== null) {
    if (!isRecord(manifest.workflow)) {
      errors.push(`${manifestFile}: workflow must be a mapping`);
    } else {
      if (typeof manifest.workflow.id !== "string" || manifest.workflow.id.trim() === "") {
        errors.push(`${manifestFile}: workflow.id must be a non-empty string`);
      }
      const norm = normalizeSafeRelativePath(manifest.workflow.path);
      if (!norm.ok) {
        errors.push(`${manifestFile}: workflow.path: ${norm.error}`);
      } else {
        if (!norm.path.endsWith(".hya.md")) {
          errors.push(`${manifestFile}: workflow.path must end with .hya.md (got ${norm.path})`);
        }
        if (!fileMap.has(norm.path)) {
          errors.push(`${manifestFile}: workflow.path references missing file: ${norm.path}`);
        }
      }
    }
  }

  // Validate APIs
  const processSpec = extractProcessSpec(manifest);
  if (manifest.apis !== undefined) {
    if (!Array.isArray(manifest.apis)) {
      errors.push(`${manifestFile}: apis must be a list`);
    } else {
      if (manifest.apis.length > 0 && !processSpec) {
        errors.push(`${manifestFile}: apis require an explicit process runtime`);
      }
      const seenApis = new Set();
      for (let i = 0; i < manifest.apis.length; i++) {
        const api = manifest.apis[i];
        const where = `apis[${i}]`;
        if (!isRecord(api)) {
          errors.push(`${where} must be a mapping`);
          continue;
        }
        if (typeof api.id !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(api.id)) {
          errors.push(`${where}.id is invalid: ${JSON.stringify(api.id)}`);
        } else if (seenApis.has(api.id)) {
          errors.push(`${where}.id duplicate api id \`${api.id}\``);
        } else {
          seenApis.add(api.id);
        }
        if (!ALLOWED_API_METHODS.has(api.method)) {
          errors.push(`${where}.method must be GET/POST/PUT/PATCH/DELETE`);
        }
        if (!ALLOWED_API_SCOPES.has(api.scope)) {
          errors.push(`${where}.scope must be session or global`);
        }
        if (typeof api.path !== "string" || !api.path.startsWith("/")) {
          errors.push(`${where}.path must start with '/'`);
        }
        for (const schemaField of ["request_schema", "response_schema"]) {
          if (api[schemaField] !== undefined && api[schemaField] !== null) {
            const norm = normalizeSafeRelativePath(api[schemaField]);
            if (!norm.ok) {
              errors.push(`${where}.${schemaField}: ${norm.error}`);
            } else if (!fileMap.has(norm.path)) {
              errors.push(`${where}.${schemaField} references missing file: ${norm.path}`);
            } else {
              try {
                JSON.parse(readFileSync(fileMap.get(norm.path).full, "utf8"));
              } catch (err) {
                errors.push(`${where}.${schemaField} (${norm.path}) is not valid JSON: ${err instanceof Error ? err.message : String(err)}`);
              }
            }
          }
        }
      }
    }
  }

  // Validate TUI extension
  if (manifest.tui !== undefined && manifest.tui !== null) {
    if (!isRecord(manifest.tui)) {
      errors.push(`${manifestFile}: tui must be a mapping`);
    } else {
      if (!Number.isInteger(manifest.tui.api_version) || manifest.tui.api_version < 1) {
        errors.push(`${manifestFile}: tui.api_version must be a positive integer`);
      }
      const norm = normalizeSafeRelativePath(manifest.tui.entry);
      if (!norm.ok) errors.push(`${manifestFile}: tui.entry: ${norm.error}`);
      else if (!fileMap.has(norm.path)) errors.push(`${manifestFile}: tui.entry references missing file: ${norm.path}`);
      if (typeof manifest.tui.sdk !== "string" || manifest.tui.sdk.trim() === "") {
        errors.push(`${manifestFile}: tui.sdk must be a non-empty string`);
      }
      if (
        manifest.tui.permissions !== undefined &&
        (!Array.isArray(manifest.tui.permissions) || !manifest.tui.permissions.every((p) => typeof p === "string"))
      ) {
        errors.push(`${manifestFile}: tui.permissions must be a list of strings`);
      }
    }
  }

  // Check config.example.yml (if present) for accidentally committed real keys
  if (fileMap.has("config.example.yml")) {
    const rawExample = readFileSync(fileMap.get("config.example.yml").full, "utf8");
    const nonCommentLines = rawExample
      .split(/\r?\n/)
      .filter((line) => !line.trim().startsWith("#"))
      .join("\n");
    scanTextForSecrets(nonCommentLines, "config.example.yml", errors);
  }

  return {
    ok: errors.length === 0,
    errors,
    warnings,
    manifest,
    classification: classifyBundle(absDir, manifest),
    processSpec,
  };
}

function runCommand(command, args, options = {}) {
  return new Promise((resolvePromise) => {
    const child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env ?? process.env,
      stdio: ["ignore", "pipe", "pipe"],
      shell: false,
    });
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    let settled = false;

    const timer =
      options.timeoutMs > 0
        ? setTimeout(() => {
            timedOut = true;
            child.kill("SIGKILL");
          }, options.timeoutMs)
        : null;

    child.stdout?.on("data", (chunk) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr?.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });
    child.on("error", (err) => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      resolvePromise({ ok: false, code: null, signal: null, timedOut, stdout, stderr, error: err.message });
    });
    child.on("close", (code, signal) => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      resolvePromise({ ok: !timedOut && code === 0, code, signal, timedOut, stdout, stderr, error: null });
    });
  });
}

function tailLines(text, maxLines = 40) {
  const lines = text.trimEnd().split(/\r?\n/).filter(Boolean);
  return lines.slice(-maxLines).join("\n");
}

export async function runBundleSelfCheck(bundleDir, manifest) {
  if (!manifest?.check?.command) {
    return { skipped: true, ok: true, detail: "no check.command declared" };
  }
  const absDir = resolve(bundleDir);
  const [program, ...args] = manifest.check.command;
  const timeoutSecs = manifest.check.timeout_secs ?? 120;
  const res = await runCommand(program, args, {
    cwd: absDir,
    timeoutMs: timeoutSecs * 1000,
    env: {
      ...process.env,
      HYA_BUNDLE_ID: String(manifest.identity?.id ?? ""),
      HYA_BUNDLE_VERSION: String(manifest.identity?.version ?? ""),
      HYA_BUNDLE_ROOT: absDir,
    },
  });
  if (!res.ok) {
    const reason = res.timedOut
      ? `timed out after ${timeoutSecs}s`
      : res.error
        ? `spawn failed: ${res.error}`
        : `exited with code=${res.code} signal=${res.signal}`;
    const outputTail = tailLines(`${res.stdout}\n${res.stderr}`);
    return {
      skipped: false,
      ok: false,
      command: manifest.check.command.join(" "),
      error: `check \`${manifest.check.command.join(" ")}\` failed (${reason})${outputTail ? `\n${outputTail}` : ""}`,
    };
  }
  return { skipped: false, ok: true, command: manifest.check.command.join(" ") };
}

function sortedStrings(values) {
  return [...new Set(values)].sort((a, b) => a.localeCompare(b));
}

function arraysEqual(a, b) {
  return a.length === b.length && a.every((val, idx) => val === b[idx]);
}

export async function runProtocolSmoke(bundleDir, manifest, options = {}) {
  const processSpec = extractProcessSpec(manifest);
  if (!processSpec || processSpec.kind !== "bun") {
    return { skipped: true, ok: true, detail: "not a JS process bundle" };
  }

  const timeoutMs = options.timeoutMs ?? 5000;
  const tmpBase = mkdtempSync(join(tmpdir(), "hya-bundle-smoke-"));
  const bundleCopy = join(tmpBase, "bundle");
  const configDir = join(tmpBase, "config");
  const configFile = join(configDir, "config.yml");
  cpSync(resolve(bundleDir), bundleCopy, { recursive: true });
  mkdirSync(configDir, { recursive: true });
  // Ensure configDir exists and contains a minimal YAML mapping.
  writeFileSync(configFile, "{}\n", "utf8");

  const expand = (arg) =>
    arg
      .replaceAll("${BUNDLE_ROOT}", bundleCopy)
      .replaceAll("${BUNDLE_CONFIG_FILE}", configFile)
      .replaceAll("${BUNDLE_CONFIG_DIR}", configDir);

  const argv = processSpec.command.map(expand);
  const [program, ...args] = argv;

  let child = null;
  try {
    child = spawn(program, args, {
      cwd: bundleCopy,
      env: {
        ...process.env,
        HYA_BUNDLE_ROOT: bundleCopy,
        HYA_BUNDLE_CONFIG_FILE: configFile,
        HYA_BUNDLE_CONFIG_DIR: configDir,
      },
      stdio: ["pipe", "pipe", "pipe"],
      shell: false,
    });

    let stderr = "";
    child.stderr?.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });

    const lineQueue = [];
    const lineWaiters = [];
    let stdoutBuf = "";
    let childClosed = false;
    let closeInfo = null;
    const closePromise = new Promise((resolveClose) => {
      child.on("close", (code, signal) => {
        childClosed = true;
        closeInfo = { code, signal };
        while (lineWaiters.length > 0) {
          lineWaiters.shift().reject(new Error(`process exited prematurely (code=${code}, signal=${signal})${stderr ? `\nstderr:\n${tailLines(stderr)}` : ""}`));
        }
        resolveClose(closeInfo);
      });
      child.on("error", (err) => {
        childClosed = true;
        closeInfo = { code: null, signal: null, error: err.message };
        while (lineWaiters.length > 0) {
          lineWaiters.shift().reject(new Error(`spawn error: ${err.message}`));
        }
        resolveClose(closeInfo);
      });
    });

    child.stdout?.on("data", (chunk) => {
      stdoutBuf += chunk.toString("utf8");
      let nl;
      while ((nl = stdoutBuf.indexOf("\n")) >= 0) {
        const line = stdoutBuf.slice(0, nl).trim();
        stdoutBuf = stdoutBuf.slice(nl + 1);
        if (line === "") continue;
        if (lineWaiters.length > 0) {
          lineWaiters.shift().resolve(line);
        } else {
          lineQueue.push(line);
        }
      }
    });

    const nextLine = (label) =>
      new Promise((resolveLine, rejectLine) => {
        if (lineQueue.length > 0) {
          resolveLine(lineQueue.shift());
          return;
        }
        if (childClosed) {
          rejectLine(new Error(`process closed before ${label} reply (code=${closeInfo?.code})`));
          return;
        }
        const timer = setTimeout(() => {
          const idx = lineWaiters.indexOf(waiter);
          if (idx >= 0) lineWaiters.splice(idx, 1);
          rejectLine(new Error(`timed out after ${timeoutMs}ms waiting for ${label} reply${stderr ? `\nstderr:\n${tailLines(stderr)}` : ""}`));
        }, timeoutMs);
        const waiter = {
          resolve: (val) => {
            clearTimeout(timer);
            resolveLine(val);
          },
          reject: (err) => {
            clearTimeout(timer);
            rejectLine(err);
          },
        };
        lineWaiters.push(waiter);
      });

    // 1. initialize
    const initFrame = {
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: { protocol_version: 1, host: { name: "hya-bundle-ci", version: "1.0.0" } },
    };
    child.stdin.write(`${JSON.stringify(initFrame)}\n`);
    const initRaw = await nextLine("initialize");
    let initReply;
    try {
      initReply = JSON.parse(initRaw);
    } catch {
      throw new Error(`stdout emitted non-JSON line during initialize: ${JSON.stringify(initRaw)}`);
    }

    if (initReply.jsonrpc !== "2.0" || initReply.id !== 1 || initReply.error) {
      throw new Error(`invalid initialize reply: ${JSON.stringify(initReply)}`);
    }
    const result = initReply.result;
    if (!isRecord(result)) {
      throw new Error(`initialize result must be an object: ${JSON.stringify(initReply)}`);
    }
    if (result.protocol_version !== 1) {
      throw new Error(`initialize protocol_version must be 1 (got ${JSON.stringify(result.protocol_version)})`);
    }
    if (!isRecord(result.plugin) || typeof result.plugin.id !== "string" || result.plugin.id.trim() === "") {
      throw new Error(`initialize result.plugin.id must be a non-empty string`);
    }

    // Verify declared tools/hooks/apis match bundle.yaml
    const declaredTools = sortedStrings((manifest.resources?.tools ?? []).map((t) => t.id));
    const reportedTools = sortedStrings(
      (Array.isArray(result.tools) ? result.tools : []).map((t) => (typeof t === "string" ? t : t?.name)).filter(Boolean),
    );
    if (!arraysEqual(declaredTools, reportedTools)) {
      throw new Error(
        `initialize tools mismatch: manifest declares [${declaredTools.join(", ")}], process reported [${reportedTools.join(", ")}]`,
      );
    }

    const declaredHooks = sortedStrings((manifest.resources?.hooks ?? []).map((h) => h.id));
    const reportedHooks = sortedStrings(
      (Array.isArray(result.hooks) ? result.hooks : []).map((h) => (typeof h === "string" ? h : h?.name)).filter(Boolean),
    );
    if (!arraysEqual(declaredHooks, reportedHooks)) {
      throw new Error(
        `initialize hooks mismatch: manifest declares [${declaredHooks.join(", ")}], process reported [${reportedHooks.join(", ")}]`,
      );
    }

    const declaredApis = sortedStrings((manifest.apis ?? []).map((a) => a.id));
    const reportedApis = sortedStrings(
      (Array.isArray(result.apis) ? result.apis : [])
        .map((a) => (typeof a === "string" ? a : (a?.name ?? a?.id)))
        .filter(Boolean),
    );
    if (!arraysEqual(declaredApis, reportedApis)) {
      throw new Error(
        `initialize apis mismatch: manifest declares [${declaredApis.join(", ")}], process reported [${reportedApis.join(", ")}]`,
      );
    }

    // 2. shutdown
    const shutdownFrame = { jsonrpc: "2.0", id: 2, method: "shutdown", params: {} };
    child.stdin.write(`${JSON.stringify(shutdownFrame)}\n`);
    const shutdownRaw = await nextLine("shutdown");
    let shutdownReply;
    try {
      shutdownReply = JSON.parse(shutdownRaw);
    } catch {
      throw new Error(`stdout emitted non-JSON line during shutdown: ${JSON.stringify(shutdownRaw)}`);
    }
    if (shutdownReply.jsonrpc !== "2.0" || shutdownReply.id !== 2 || shutdownReply.error) {
      throw new Error(`invalid shutdown reply: ${JSON.stringify(shutdownReply)}`);
    }

    // Close stdin so processes that exit on EOF (like model-fallback) terminate cleanly.
    child.stdin.end();

    const exitResult = await Promise.race([
      closePromise,
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error(`process did not exit within ${timeoutMs}ms after shutdown`)), timeoutMs),
      ),
    ]);
    if (exitResult.code !== 0 || exitResult.signal !== null) {
      throw new Error(
        `process did not exit cleanly after shutdown (code=${exitResult.code}, signal=${exitResult.signal})${stderr ? `\nstderr:\n${tailLines(stderr)}` : ""}`,
      );
    }

    return {
      skipped: false,
      ok: true,
      plugin: result.plugin,
      tools: reportedTools,
      hooks: reportedHooks,
      apis: reportedApis,
    };
  } catch (error) {
    if (child && !child.killed) {
      try {
        child.kill("SIGKILL");
      } catch {
        // ignore
      }
    }
    return {
      skipped: false,
      ok: false,
      error: error instanceof Error ? error.message : String(error),
    };
  } finally {
    rmSync(tmpBase, { recursive: true, force: true });
  }
}

function splitCommandTemplate(template, vars) {
  // Simple shell-like tokenization supporting single/double quotes without invoking a shell.
  const tokens = [];
  let cur = "";
  let quote = null;
  for (let i = 0; i < template.length; i++) {
    const ch = template[i];
    if (quote) {
      if (ch === quote) quote = null;
      else cur += ch;
      continue;
    }
    if (ch === "'" || ch === '"') {
      quote = ch;
    } else if (/\s/.test(ch)) {
      if (cur !== "") {
        tokens.push(cur);
        cur = "";
      }
    } else {
      cur += ch;
    }
  }
  if (cur !== "") tokens.push(cur);
  return tokens.map((tok) =>
    tok
      .replaceAll("{source}", vars.source)
      .replaceAll("{output}", vars.output),
  );
}

function buildPackInvocation(packCmd, source, output) {
  if (packCmd.includes("{source}") || packCmd.includes("{output}")) {
    const argv = splitCommandTemplate(packCmd, { source, output });
    return { program: argv[0], args: argv.slice(1), inferredVerify: null };
  }
  const tokens = splitCommandTemplate(packCmd, { source, output });
  const bin = tokens[0];
  const base = basename(bin).replace(/\.exe$/i, "");
  if (tokens.length === 1 && base === "hya") {
    return {
      program: bin,
      args: ["bundle", "pack", source, output],
      inferredVerify: { program: bin, args: ["bundle", "verify", output] },
    };
  }
  if (tokens.length === 1 && base === "xtask") {
    return {
      program: bin,
      args: ["package-bundle", source, output],
      inferredVerify: null,
    };
  }
  return { program: tokens[0], args: [...tokens.slice(1), source, output], inferredVerify: null };
}

export async function runPackDeterminismCheck(bundleDir, options = {}) {
  const { packCmd, verifyCmd, timeoutMs = 120_000 } = options;
  if (!packCmd) {
    return { skipped: true, ok: true, detail: "no --pack-cmd provided" };
  }
  const absSource = resolve(bundleDir);
  const tmpBase = mkdtempSync(join(tmpdir(), "hya-bundle-pack-"));
  const firstOut = join(tmpBase, "first.hyabundle");
  const secondOut = join(tmpBase, "second.hyabundle");

  try {
    const firstInv = buildPackInvocation(packCmd, absSource, firstOut);
    const r1 = await runCommand(firstInv.program, firstInv.args, { timeoutMs });
    if (!r1.ok) {
      throw new Error(`first pack failed (code=${r1.code}): ${tailLines(`${r1.stdout}\n${r1.stderr}`)}`);
    }
    const secondInv = buildPackInvocation(packCmd, absSource, secondOut);
    const r2 = await runCommand(secondInv.program, secondInv.args, { timeoutMs });
    if (!r2.ok) {
      throw new Error(`second pack failed (code=${r2.code}): ${tailLines(`${r2.stdout}\n${r2.stderr}`)}`);
    }

    const bytes1 = readFileSync(firstOut);
    const bytes2 = readFileSync(secondOut);
    if (bytes1.length === 0) throw new Error("packed .hyabundle is empty");
    if (bytes1.length > MAX_PACKAGE_BYTES) {
      throw new Error(`packed .hyabundle exceeds 128 MiB limit (${bytes1.length} bytes)`);
    }
    if (!bytes1.subarray(0, PUBLIC_7Z_MAGIC.length).equals(PUBLIC_7Z_MAGIC)) {
      throw new Error("packed .hyabundle does not start with 7z PUBLIC_MAGIC header");
    }

    const sha1 = createHash("sha256").update(bytes1).digest("hex");
    const sha2 = createHash("sha256").update(bytes2).digest("hex");
    if (sha1 !== sha2) {
      throw new Error(`non-deterministic pack output: sha256 #1=${sha1} != sha256 #2=${sha2}`);
    }

    let verifiedArchive = false;
    let verifyInv = null;
    if (verifyCmd) {
      const argv = splitCommandTemplate(verifyCmd, { source: absSource, output: firstOut });
      if (!verifyCmd.includes("{output}")) argv.push(firstOut);
      verifyInv = { program: argv[0], args: argv.slice(1) };
    } else if (firstInv.inferredVerify) {
      verifyInv = firstInv.inferredVerify;
    }

    if (verifyInv) {
      const vr = await runCommand(verifyInv.program, verifyInv.args, { timeoutMs });
      if (!vr.ok) {
        throw new Error(`archive verify failed (code=${vr.code}): ${tailLines(`${vr.stdout}\n${vr.stderr}`)}`);
      }
      verifiedArchive = true;
    }

    return {
      skipped: false,
      ok: true,
      sha256: sha1,
      bytes: bytes1.length,
      verified_archive: verifiedArchive,
    };
  } catch (error) {
    return {
      skipped: false,
      ok: false,
      error: error instanceof Error ? error.message : String(error),
    };
  } finally {
    rmSync(tmpBase, { recursive: true, force: true });
  }
}

export async function verifyBundle(bundleDir, options = {}) {
  const staticResult = validateBundleStatic(bundleDir);
  const report = {
    path: toPosix(relative(process.cwd(), resolve(bundleDir))) || ".",
    id: staticResult.manifest?.identity?.id ?? null,
    type: staticResult.classification?.type ?? null,
    static: { ok: staticResult.ok, errors: staticResult.errors, warnings: staticResult.warnings },
    check: { skipped: true, ok: true },
    protocol: { skipped: true, ok: true },
    pack: { skipped: true, ok: true },
    ok: staticResult.ok,
  };

  if (!staticResult.ok) {
    return report;
  }

  if (!options.skipCheck) {
    report.check = await runBundleSelfCheck(bundleDir, staticResult.manifest);
    if (!report.check.ok) report.ok = false;
  }

  if (!options.skipProtocol) {
    report.protocol = await runProtocolSmoke(bundleDir, staticResult.manifest, {
      timeoutMs: options.protocolTimeoutMs ?? 5000,
    });
    if (!report.protocol.ok) report.ok = false;
  }

  if (options.packCmd) {
    report.pack = await runPackDeterminismCheck(bundleDir, {
      packCmd: options.packCmd,
      verifyCmd: options.verifyCmd,
    });
    if (!report.pack.ok) report.ok = false;
  }

  return report;
}

export function formatGithubOutputs(discovery) {
  return [
    `matrix_all=${JSON.stringify({ include: discovery.all })}`,
    `matrix_native=${JSON.stringify({ include: discovery.native })}`,
    `matrix_js_static=${JSON.stringify({ include: discovery.js_static })}`,
    `has_all=${discovery.has_all ? "true" : "false"}`,
    `has_native=${discovery.has_native ? "true" : "false"}`,
    `has_js_static=${discovery.has_js_static ? "true" : "false"}`,
    `count_all=${discovery.all.length}`,
    `count_native=${discovery.native.length}`,
    `count_js_static=${discovery.js_static.length}`,
  ].join("\n") + "\n";
}

export { posix };
