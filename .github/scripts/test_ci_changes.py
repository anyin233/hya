#!/usr/bin/env python3
"""Unit tests for `.github/scripts/ci_changes.py`."""

from __future__ import annotations

import unittest

from ci_changes import ALL_SCOPES, ScopeResult, classify_paths


class ClassifyPathsTests(unittest.TestCase):
    def test_pure_website_runs_web_and_assembled_docs_only(self) -> None:
        res = classify_paths(["public/index.html", "public/style.css", "public/assets/hya-logo.png"])
        self.assertEqual(res, ScopeResult(docs=True, web=True))
        self.assertFalse(res.rust)
        self.assertFalse(res.frontend)
        self.assertFalse(res.build_bin)
        self.assertFalse(res.tui)

    def test_pure_docs_only_runs_docs(self) -> None:
        res = classify_paths(["docs/guide/workflows.md", "docs/site/astro.config.mjs"])
        self.assertEqual(res, ScopeResult(docs=True))
        self.assertFalse(res.web)
        self.assertFalse(res.build_bin)
        self.assertFalse(res.tui)

    def test_pure_meta_docs_skip_everything(self) -> None:
        res = classify_paths([
            "README.md",
            "AGENTS.md",
            ".agents/spec/index.md",
            ".planning/note.md",
            ".github/workflows/install-smoke.yml",
        ])
        self.assertEqual(res, ScopeResult())
        self.assertFalse(res.build_bin)
        self.assertFalse(res.tui)

    def test_pure_frontend_skips_rust_heavy_jobs_but_runs_build_bin_and_tui(self) -> None:
        res = classify_paths([
            "packages/hya-tui/src/app.tsx",
            "packages/hya-tui-web/src/main.ts",
            "packages/hya-tui-sdk/src/main.ts",
        ])
        self.assertEqual(res, ScopeResult(frontend=True))
        self.assertTrue(res.build_bin)
        self.assertTrue(res.tui)
        self.assertFalse(res.rust)

    def test_pure_rust_internal_runs_rust_and_tui_but_skips_frontend(self) -> None:
        res = classify_paths(["crates/hya-store/src/lib.rs", "crates/hya-engine/src/turn.rs"])
        self.assertEqual(res, ScopeResult(rust=True))
        self.assertTrue(res.build_bin)
        self.assertTrue(res.tui)
        self.assertFalse(res.frontend)

    def test_backend_contract_crates_trigger_both_rust_and_frontend(self) -> None:
        for path in [
            "crates/hya-server/src/v1/sessions.rs",
            "crates/hya-api/src/lib.rs",
            "crates/hya-proto/src/event.rs",
            "crates/hya-backend/src/main.rs",
            "crates/hya-plugin/src/messages.rs",
            "crates/hya-plugin-bun/adapter/src/main.ts",
            "crates/hya-plugin-claude/adapter/src/main.ts",
            "versions.toml",
            "packages/hya-tui/frontend-version.ts",
            "packages/hya-tui/package.json",
            "packages/hya-tui/src/operations.json",
            "packages/hya-tui/src/proto/hya/v1/session.proto",
        ]:
            with self.subTest(path=path):
                res = classify_paths([path])
                self.assertTrue(res.rust)
                self.assertTrue(res.frontend)
                self.assertTrue(res.build_bin)
                self.assertTrue(res.tui)

    def test_v1_proto_triggers_rust_frontend_and_docs(self) -> None:
        res = classify_paths(["proto/hya/v1/session.proto"])
        self.assertEqual(res, ScopeResult(rust=True, frontend=True, docs=True))

    def test_generated_protocol_docs_trigger_rust_and_docs(self) -> None:
        res = classify_paths(["docs/protocol/openapi.json"])
        self.assertEqual(res, ScopeResult(rust=True, docs=True))

    def test_changelog_triggers_rust_for_xtask_validation(self) -> None:
        res = classify_paths(["CHANGELOG.md"])
        self.assertEqual(res, ScopeResult(rust=True))

    def test_ci_workflow_change_runs_all_scopes(self) -> None:
        res = classify_paths([".github/workflows/ci.yml"])
        self.assertEqual(res, ALL_SCOPES)

    def test_docs_workflow_change_runs_docs(self) -> None:
        res = classify_paths([".github/workflows/docs.yml"])
        self.assertEqual(res, ScopeResult(docs=True))

    def test_web_checker_script_runs_web(self) -> None:
        res = classify_paths([".github/scripts/check_web.py"])
        self.assertEqual(res, ScopeResult(web=True))

    def test_unknown_file_fails_safe_to_code_checks(self) -> None:
        res = classify_paths(["new-crate/src/lib.rs"])
        self.assertTrue(res.rust)
        self.assertTrue(res.frontend)
        self.assertFalse(res.docs)
        self.assertFalse(res.web)

    def test_mixed_web_and_docs_skips_code(self) -> None:
        res = classify_paths(["public/index.html", "docs/README.md"])
        self.assertEqual(res, ScopeResult(docs=True, web=True))
        self.assertFalse(res.rust)
        self.assertFalse(res.frontend)
        self.assertFalse(res.build_bin)


if __name__ == "__main__":
    unittest.main()
