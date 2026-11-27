#!/usr/bin/env node
// Verify one or more Hya bundle source directories:
//   1. Static manifest, descriptor, path-safety, and secret-hygiene checks
//   2. Declared `check.command` self-check execution
//   3. Black-box JSON-RPC 2.0 protocol smoke test for JS process bundles
//   4. Optional deterministic `.hyabundle` packaging & verification (`--pack-cmd`)

import { verifyBundle } from "./lib.mjs";

function usage() {
  console.error(
    "Usage: verify.mjs [--skip-check] [--skip-protocol] [--pack-cmd <cmd>] [--verify-cmd <cmd>] [--json] <bundle-dir> [<bundle-dir>...]",
  );
  process.exit(2);
}

function parseArgs(argv) {
  const dirs = [];
  let skipCheck = false;
  let skipProtocol = false;
  let packCmd = null;
  let verifyCmd = null;
  let jsonOutput = false;

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") {
      usage();
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

  if (dirs.length === 0) usage();
  return { dirs, skipCheck, skipProtocol, packCmd, verifyCmd, jsonOutput };
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const reports = [];
  let allOk = true;

  for (const dir of opts.dirs) {
    const report = await verifyBundle(dir, {
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
