#!/usr/bin/env python3
"""Classify changed repository paths into on-demand GitHub Actions CI scopes.

Design principles:
1. Asymmetric dependency (Frontend -> Backend):
   - Pure Bun/TUI frontend changes run `frontend` + `build-bin` + `tui` and skip
     heavy Rust-only checks (`lint`, `test`, `e2e`).
   - Pure Rust internal changes run `lint`, `test`, `build-bin`, `e2e`, and
     keep `tui` (real `hya` binary <-> precompiled TUI integration) so implicit
     backend behavioral breaks are always caught.
   - Cross-boundary contract/adapter paths (`proto/`, `hya-api`, `hya-proto`,
     `hya-server`, `hya-backend`, `hya-plugin*`, `versions.toml`, generated
     contract files, `packages/*/package.json`) trigger both Rust and Frontend.
2. Pages (`public/` + `docs/`) isolation:
   - `public/**` (root landing page) and `docs/**` (Starlight documentation
     mounted under `/docs/`, excluding generated `docs/protocol/*`) form one
     unified GitHub Pages site (`dist-pages`) and run only the `docs` job
     (`check_web.py` fast-fail + Starlight build + assembled Pages link check),
     skipping all Rust/Frontend/TUI code builds.
   - Pure repository meta-docs (`README.md`, `AGENTS.md`, `.agents/**`, etc.)
     skip all heavy jobs and let `ci-gate` pass immediately.
3. Fail-safe default:
   - Any unrecognised file not explicitly classified as non-CI falls back to
     running Rust + Frontend checks so new code directories are never skipped.
"""

from __future__ import annotations

import argparse
import os
import posixpath
import subprocess
import sys
from dataclasses import dataclass

ZERO_SHA = "0" * 40


@dataclass(frozen=True)
class ScopeResult:
    rust: bool = False
    frontend: bool = False
    docs: bool = False

    @property
    def build_bin(self) -> bool:
        return self.rust or self.frontend

    @property
    def tui(self) -> bool:
        return self.rust or self.frontend

    def as_dict(self) -> dict[str, bool]:
        return {
            "rust": self.rust,
            "frontend": self.frontend,
            "build_bin": self.build_bin,
            "tui": self.tui,
            "docs": self.docs,
        }


ALL_SCOPES = ScopeResult(rust=True, frontend=True, docs=True)

# Files that trigger EVERY CI scope (workflow/classifier changes).
FULL_CI_FILES = {
    ".github/workflows/ci.yml",
    ".github/scripts/ci_changes.py",
}

# Exact repository files that never affect any CI gate.
IGNORED_EXACT_FILES = {
    ".gitattributes",
    ".gitignore",
    ".github/PULL_REQUEST_TEMPLATE.md",
    ".github/workflows/builtin-bundles.yml",
    ".github/workflows/bundles.yml",
    ".github/workflows/install-smoke.yml",
    ".github/workflows/nightly.yml",
    ".github/workflows/release.yml",
    "AGENTS.md",
    "CLAUDE.md",
    "CONTEXT.md",
    "README.md",
    "THIRD_PARTY_NOTICES",
    "opencode.json",
}

# Directories that never affect the main `ci.yml` gate (meta-docs or covered by
# independent bundle workflows such as `.github/workflows/bundles.yml`).
IGNORED_PREFIXES = (
    ".agent-docs/",
    ".agents/",
    ".planning/",
    "bundles/examples/",
    "bundles/extra/",
    "scripts/bundle-ci/",
)

# Generated docs checked by both `xtask gen-api` drift check and the docs site.
GENERATED_PROTOCOL_DOCS = {
    "docs/protocol/api-reference.md",
    "docs/protocol/openapi.json",
}

# Cross-boundary Rust crates / plugin adapters where backend & frontend meet.
CONTRACT_PREFIXES = (
    "bundles/builtin/basic-tui-components/",
    "crates/hya-api/",
    "crates/hya-backend/",
    "crates/hya-plugin/",
    "crates/hya-plugin-bun/",
    "crates/hya-plugin-claude/",
    "crates/hya-proto/",
    "crates/hya-server/",
    "packages/hya-tui/src/proto/",
    ".github/actions/setup-bun/",
)

# Specific files whose contents are checked across Rust (`xtask`) and Frontend.
CONTRACT_EXACT_FILES = {
    "versions.toml",
    "packages/hya-tui/frontend-version.ts",
    "packages/hya-tui/src/operations.json",
    "crates/xtask/src/gen_api.rs",
    "crates/xtask/src/gen_plugin.rs",
}

# Pure Rust / backend directories and root files (internal changes still keep
# `tui` via `rust=True` so implicit backend <-> TUI regressions are caught).
RUST_ONLY_PREFIXES = (
    ".config/",
    "bundles/builtin/",
    "crates/",
    "plugins/",
    "proto/",
    "scripts/",
    "tests/",
    "vendor/",
)

RUST_ONLY_EXACT_FILES = {
    "CHANGELOG.md",
    "Cargo.lock",
    "Cargo.toml",
    "clippy.toml",
    "install.sh",
    "rustfmt.toml",
}


def normalize_path(raw: str) -> str:
    cleaned = raw.strip().replace("\\", "/")
    while cleaned.startswith("./"):
        cleaned = cleaned[2:]
    return posixpath.normpath(cleaned) if cleaned else ""


def is_package_manifest(path: str) -> bool:
    parts = path.split("/")
    return len(parts) == 3 and parts[0] == "packages" and parts[2] == "package.json"


def classify_paths(paths: list[str]) -> ScopeResult:
    rust = False
    frontend = False
    docs = False

    for raw in paths:
        path = normalize_path(raw)
        if not path or path == ".":
            continue

        # 1. CI self-changes -> run everything.
        if path in FULL_CI_FILES:
            return ALL_SCOPES

        # 2. Classifier unit tests run inside `changes` itself; no extra job needed.
        if path == ".github/scripts/test_ci_changes.py":
            continue

        # 3. GitHub Pages site (`public/` landing page + `docs/` Starlight site).
        if (
            path == ".github/scripts/check_web.py"
            or path == ".github/workflows/docs.yml"
            or path.startswith("public/")
        ):
            docs = True
            continue
        if path in GENERATED_PROTOCOL_DOCS:
            rust = True
            docs = True
            continue
        if path.startswith("docs/"):
            docs = True
            continue

        # 6. Explicitly ignored non-CI meta files / directories.
        if path in IGNORED_EXACT_FILES or path.startswith(IGNORED_PREFIXES):
            continue

        # 7. v1 proto definitions generate Rust, TUI (`operations.json` + proto),
        #    and `docs/protocol/*` -> trigger Rust, Frontend, and Docs.
        if path.startswith("proto/hya/v1/"):
            rust = True
            frontend = True
            docs = True
            continue

        # 8. Cross-boundary contract / adapter / manifest files -> Rust + Frontend.
        if (
            path in CONTRACT_EXACT_FILES
            or path.startswith(CONTRACT_PREFIXES)
            or is_package_manifest(path)
        ):
            rust = True
            frontend = True
            continue

        # 9. Pure Bun / TUI frontend packages -> Frontend (+ build-bin + tui).
        if path.startswith("packages/"):
            frontend = True
            continue

        # 10. Pure Rust / backend paths -> Rust (+ build-bin + tui).
        if path in RUST_ONLY_EXACT_FILES or path.startswith(RUST_ONLY_PREFIXES):
            rust = True
            continue

        # 11. Fail-safe: any unknown top-level file triggers code checks.
        rust = True
        frontend = True

    return ScopeResult(rust=rust, frontend=frontend, docs=docs)


def scope_from_dispatch(scope: str) -> ScopeResult | None:
    mapping = {
        "all": ALL_SCOPES,
        "rust": ScopeResult(rust=True),
        "frontend": ScopeResult(frontend=True),
        "docs": ScopeResult(docs=True),
        "web": ScopeResult(docs=True),
    }
    return mapping.get(scope)


def run_git(args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def commit_exists(ref: str) -> bool:
    if not ref or ref == ZERO_SHA:
        return False
    return run_git(["cat-file", "-e", f"{ref}^{{commit}}"]).returncode == 0


def diff_files(rev_spec: str) -> list[str] | None:
    res = run_git(["diff", "--no-renames", "--name-only", rev_spec])
    if res.returncode != 0:
        print(f"git diff {rev_spec} failed: {res.stderr.strip()}", file=sys.stderr)
        return None
    return [line for line in res.stdout.splitlines() if line.strip()]


def collect_changed_files(
    event_name: str,
    base_sha: str,
    head_sha: str,
    before_sha: str,
) -> tuple[list[str] | None, str]:
    if event_name == "pull_request":
        if commit_exists(base_sha) and commit_exists(head_sha):
            files = diff_files(f"{base_sha}...{head_sha}")
            if files is not None:
                return files, f"pull_request diff {base_sha[:12]}...{head_sha[:12]}"
        return None, "pull_request commits unavailable (fail-safe: run all)"

    if event_name == "push":
        if commit_exists(before_sha) and commit_exists(head_sha):
            files = diff_files(f"{before_sha}..{head_sha}")
            if files is not None:
                return files, f"push diff {before_sha[:12]}..{head_sha[:12]}"
        return None, "push base commit unavailable or initial/force push (fail-safe: run all)"

    if event_name == "workflow_dispatch":
        if commit_exists("origin/main") and commit_exists("HEAD"):
            files = diff_files("origin/main...HEAD")
            if files is not None and files:
                return files, "workflow_dispatch auto diff origin/main...HEAD"
        if commit_exists("HEAD~1") and commit_exists("HEAD"):
            files = diff_files("HEAD~1..HEAD")
            if files is not None:
                return files, "workflow_dispatch auto diff HEAD~1..HEAD"
        return None, "workflow_dispatch auto diff unavailable (fail-safe: run all)"

    return None, f"unhandled event '{event_name}' (fail-safe: run all)"


def write_outputs(result: ScopeResult, reason: str, files: list[str] | None) -> None:
    outputs = result.as_dict()
    github_output = os.environ.get("GITHUB_OUTPUT")
    if github_output:
        with open(github_output, "a", encoding="utf-8") as fh:
            for key, val in outputs.items():
                fh.write(f"{key}={'true' if val else 'false'}\n")

    summary_lines = [
        "### CI Scope Selection",
        "",
        f"- **Source**: {reason}",
        "- **Selected jobs**: "
        + ", ".join(f"`{k}={str(v).lower()}`" for k, v in outputs.items()),
    ]
    if files is not None:
        summary_lines.append(f"- **Changed files ({len(files)})**:")
        for path in files[:50]:
            summary_lines.append(f"  - `{path}`")
        if len(files) > 50:
            summary_lines.append(f"  - ... and {len(files) - 50} more")

    summary_text = "\n".join(summary_lines) + "\n"
    print(summary_text)

    step_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if step_summary:
        with open(step_summary, "a", encoding="utf-8") as fh:
            fh.write(summary_text)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", default=os.environ.get("GITHUB_EVENT_NAME", ""))
    parser.add_argument("--base", default=os.environ.get("PR_BASE_SHA", ""))
    parser.add_argument("--head", default=os.environ.get("PR_HEAD_SHA", os.environ.get("GITHUB_SHA", "")))
    parser.add_argument("--before", default=os.environ.get("PUSH_BEFORE_SHA", ""))
    parser.add_argument("--dispatch-scope", default=os.environ.get("DISPATCH_SCOPE", "all"))
    args = parser.parse_args()

    if args.event == "workflow_dispatch":
        explicit = scope_from_dispatch(args.dispatch_scope)
        if explicit is not None:
            write_outputs(explicit, f"workflow_dispatch scope={args.dispatch_scope}", None)
            return 0

    files, reason = collect_changed_files(args.event, args.base, args.head, args.before)
    if files is None:
        write_outputs(ALL_SCOPES, reason, None)
        return 0

    result = classify_paths(files)
    write_outputs(result, reason, files)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
