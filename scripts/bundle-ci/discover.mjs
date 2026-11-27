#!/usr/bin/env node
// Discover Hya bundle directories, classify them (native/js/tui/static),
// optionally filter by changed files, and emit a GitHub Actions matrix JSON.

import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { discoverBundles, formatGithubOutputs } from "./lib.mjs";

function usage() {
  console.error(
    "Usage: discover.mjs [--changed-files <path>] [--changed <file>] [--github-output <path>] [--allow-empty] <root-dir> [<root-dir>...]",
  );
  process.exit(2);
}

function parseArgs(argv) {
  const roots = [];
  const changed = [];
  let changedFilesPath = null;
  let githubOutput = null;
  let allowEmpty = false;

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") {
      usage();
    } else if (arg === "--allow-empty") {
      allowEmpty = true;
    } else if (arg === "--changed-files") {
      changedFilesPath = argv[++i];
      if (!changedFilesPath) usage();
    } else if (arg === "--changed") {
      const value = argv[++i];
      if (!value) usage();
      changed.push(value);
    } else if (arg === "--github-output") {
      githubOutput = argv[++i];
      if (!githubOutput) usage();
    } else if (arg.startsWith("-")) {
      console.error(`Unknown option: ${arg}`);
      usage();
    } else {
      roots.push(arg);
    }
  }

  if (roots.length === 0) usage();

  let changedFiles = null;
  if (changedFilesPath !== null || changed.length > 0) {
    changedFiles = [...changed];
    if (changedFilesPath !== null) {
      const content =
        changedFilesPath === "-"
          ? readFileSync(0, "utf8")
          : existsSync(changedFilesPath)
            ? readFileSync(changedFilesPath, "utf8")
            : "";
      for (const line of content.split(/\r?\n/)) {
        const trimmed = line.trim();
        if (trimmed) changedFiles.push(trimmed);
      }
    }
  }

  return { roots, changedFiles, githubOutput, allowEmpty };
}

function main() {
  const opts = parseArgs(process.argv.slice(2));
  const result = discoverBundles(opts.roots, {
    changedFiles: opts.changedFiles,
    allowEmpty: opts.allowEmpty,
  });

  if (opts.githubOutput) {
    appendFileSync(opts.githubOutput, formatGithubOutputs(result), "utf8");
  }

  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

main();
