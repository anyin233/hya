#!/usr/bin/env node
// Verify one or more Hya bundle source directories:
//   1. Static v2-only manifest, descriptor, path-safety, secret-hygiene, and ABI v2 red-line checks
//   2. Native ABI v2 exported symbol checks when compiled cdylibs exist or in `--strict` mode
//   3. Declared `check.command` self-check execution
//   4. Black-box JSON-RPC 2.0 protocol smoke test for JS process bundles
//   5. Optional deterministic `.hyabundle` packaging & verification (`--pack-cmd`)

import { discoverBundles, verifyBundle } from "./lib.mjs";

function usage() {
  console.error(
    "Usage: verify.mjs [--root <dir>]... [--strict] [--require-cdylib] [--cdylib <path>] [--skip-check] [--skip-protocol] [--pack-cmd <cmd>] [--verify-cmd <cmd>] [--json] [<bundle-dir>...]",
  );
  process.exit(2);
}

function parseArgs(argv) {
  const roots = [];
  const dirs = [];
  let strict = false;
  let requireCdylib = false;
  let cdylib = null;
  let skipCheck = false;
  let skipProtocol = false;
  let packCmd = null;
  let verifyCmd = null;
  let jsonOutput = false;

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") {
      usage();
    } else if (arg === "--root") {
      const value = argv[++i];
      if (!value) usage();
      roots.push(value);
    } else if (arg === "--strict") {
      strict = true;
    } else if (arg === "--require-cdylib") {
      requireCdylib = true;
    } else if (arg === "--cdylib") {
      cdylib = argv[++i];
      if (!cdylib) usage();
    } else if (arg === "--skip-check") {
      skipCheck = true;
    } else if (arg === "--skip-protocol") {
      skipProtocol = true;
    } else if (arg === "--pack-cmd") {
      packCmd = argv[++i];
      if (!packCmd) usage();
    } else if (arg === "--verify-cmd") {
      verifyCmd = argv[++i];
      if (!verifyCmd) usage();
    } else if (arg === "--json") {
      jsonOutput = true;
    } else if (arg.startsWith("-")) {
      console.error(`Unknown option: ${arg}`);
      usage();
    } else {
      dirs.push(arg);
    }
  }

  if (roots.length === 0 && dirs.length === 0) usage();
  return {
    roots,
    dirs,
    strict,
    requireCdylib,
    cdylib,
    skipCheck,
    skipProtocol,
    packCmd,
    verifyCmd,
    jsonOutput,
  };
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const targetDirs = [];
  const seen = new Set();

  if (opts.roots.length > 0) {
    const discovered = discoverBundles(opts.roots);
    for (const entry of discovered.all) {
      if (!seen.has(entry.path)) {
        seen.add(entry.path);
        targetDirs.push(entry.path);
      }
    }
  }
  for (const dir of opts.dirs) {
    if (!seen.has(dir)) {
      seen.add(dir);
      targetDirs.push(dir);
    }
  }

  if (targetDirs.length === 0) {
    console.error("No bundles found to verify.");
    process.exit(1);
  }

  const reports = [];
  let allOk = true;

  for (const dir of targetDirs) {
    const report = await verifyBundle(dir, {
      strict: opts.strict,
      requireCdylib: opts.requireCdylib,
      cdylib: opts.cdylib,
      skipCheck: opts.skipCheck,
      skipProtocol: opts.skipProtocol,
      packCmd: opts.packCmd,
      verifyCmd: opts.verifyCmd,
    });
    reports.push(report);
    if (!report.ok) allOk = false;

    if (!opts.jsonOutput) {
      const status = report.ok ? "PASS" : "FAIL";
      const id = report.id ?? "(unknown)";
      const type = report.type ?? "unknown";
      console.log(`[${status}] ${report.path} (${id}, type=${type})`);
      if (!report.static.ok) {
        for (const err of report.static.errors) console.error(`  static error: ${err}`);
      }
      if (report.native && !report.native.skipped) {
        if (report.native.ok) {
          const modeDetail = report.native.library
            ? `cdylib=${report.native.library}`
            : `mode=${report.native.mode}`;
          console.log(
            `  native ABI v2: passed (${modeDetail}, symbols=[${report.native.symbols.join(",")}])`,
          );
        } else {
          for (const err of report.native.errors ?? [report.native.error].filter(Boolean)) {
            console.error(`  native error: ${err}`);
          }
        }
      }
      if (!report.check.skipped) {
        if (report.check.ok) console.log(`  check: passed (${report.check.command})`);
        else console.error(`  check error: ${report.check.error}`);
      }
      if (!report.protocol.skipped) {
        if (report.protocol.ok) {
          console.log(
            `  protocol: passed (plugin=${report.protocol.plugin.id}, tools=[${report.protocol.tools.join(",")}], hooks=[${report.protocol.hooks.join(",")}], apis=[${report.protocol.apis.join(",")}])`,
          );
        } else {
          console.error(`  protocol error: ${report.protocol.error}`);
        }
      }
      if (!report.pack.skipped) {
        if (report.pack.ok) {
          console.log(
            `  pack: deterministic (${report.pack.bytes} bytes, sha256=${report.pack.sha256.slice(0, 12)}…, verified=${report.pack.verified_archive})`,
          );
        } else {
          console.error(`  pack error: ${report.pack.error}`);
        }
      }
    }
  }

  if (opts.jsonOutput) {
    process.stdout.write(`${JSON.stringify({ ok: allOk, reports }, null, 2)}\n`);
  }

  if (!allOk) process.exit(1);
}

await main();
