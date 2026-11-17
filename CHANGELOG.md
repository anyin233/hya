# 0.48.0-dev.3

## Full mode moves out of `hya-core`

- The full-mode session engine (`SessionEngine`, turns, compaction settings, hooks, subagents, teams, Workflows, goal/loop drivers) now lives in the new `hya-engine` crate. `hya-core` keeps only the three-primitive kernel, the internal bundle loader, and `CoreError`, and depends only on `hya-proto`, `hya-provider`, `hya-store`, `hya-bundle`, and `hya-tool`. Behavior is unchanged. See [How full mode runs on the kernel](docs/kernel.md#how-full-mode-runs-on-the-kernel).

## Internal bundles and the rewrite contract

- Internal bundles are first-party native libraries under `bundles/internal/` that carry full-mode features out of the kernel. `hya_bundle::INTERNAL_BUNDLES` lists them; full mode loads them once per process with `hya_core::bundles::load_internal()`, and `hya exec --bare` never loads them. See [Bundle Runtime](docs/bundle-runtime.md).
- `hya-tool` now holds the rewrite contract a native bundle implements: `Rewriter { before_round, rewrite_now }` and the host services it calls (`SessionLog`, `Transcript`, `ModelHost`, `CompactionHooks`). A native library registers rewriters through the optional `hya_tool_bundle_register_rewriters_v1` export. Token estimation (`hya_tool::tokens`) is shared by the host and bundles. See [Rewrite contract](docs/kernel.md#rewrite-contract).

## Compaction is the `hya/compaction` internal bundle

- The compaction ladder (`shake`, `remote`, `soft`, `snapcompact`, `handoff`) and the `/compact` / `SummarizeSession` fold now run from the `hya/compaction` internal bundle, the fourteenth first-party bundle. Order, thresholds, configuration, events, and the summarizer are unchanged; the host keeps the settings and the summarizer the bundle calls back into, and the terminal handoff. Without the bundle, full mode uses the kernel's 80% rewrite. See [Compaction](docs/compaction.md).
- The release archive and standalone assets now include `hya-compaction.hyabundle`, built per target like the native tool families.
