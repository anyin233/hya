#!/usr/bin/env python3
"""Build one Grok brief per rustdoc sub-batch from the live missing_docs output.

The item list is embedded so the writer never needs to run cargo -- the agents run
with acceptEdits, which gates shell commands. The orchestrator runs the lint to
verify, which is also why the writer's self-report is not the gate.
"""
import collections
import json
from pathlib import Path

HERE = Path(__file__).parent
ITEMS = json.load(open(HERE / "items.json"))

# sub-batch -> (crates, crate-doc instruction, priority prose)
SUB = {
    "Q1": (["hya-tool"],
           "Expand the crate-level `//!`. It is currently one line for the largest "
           "public surface in the workspace.",
           "Document these FIRST, in this order -- they are what a tool author or a "
           "security reviewer reads first:\n"
           "1. `tool.rs`: the `Tool` trait, `ToolCtx`, `ToolError`, `ToolPermission`, "
           "`ResolvedTool`.\n"
           "2. `permission.rs`: `PermissionPlane`, `PermissionRules`, `Invocation`, "
           "`InvocationPolicy`, `PermissionInterceptor`, `Action`, `Resource`, `Mode`, "
           "`PermissionModel`, `InvocationRule`, `Decision`, `RememberScope`. This is a "
           "SECURITY state machine -- describe what each value MEANS for authorization, "
           "not what it is named.\n"
           "3. The plane pattern: `SpawnerPlane`, `InteractionPlane`, `TodoPlane`, "
           "`LspPlane`, `MailboxPlane`, `SkillPlane`, `WebSearchPlane`.\n"
           "4. Everything else, including the concrete `*Tool` structs."),
    "Q3": (["hya-store"],
           "The crate-level `//!` is substantive; leave it unless it is wrong.",
           "This crate is at 0% coverage. Document `SessionStore` first (it is the "
           "entire entry point), then `StoreError`, `SessionInfo`, `LedgerEntry`, then "
           "the admission state machine (`AdmissionState`, `AdmissionRecord`, "
           "`AdmissionClaimOutcome`, `AdmissionTerminal`), then `BundleRegistry` and "
           "`MAX_ADMISSION_INTENT_BYTES`.\n\n"
           "The admission types are SAFETY-CRITICAL: they bound spawn budgets. "
           "docs/architecture/admission-and-governor.md documents this same machine -- "
           "read it and stay consistent with it."),
    "Q5": (["hya-provider"],
           "The crate-level `//!` is present; expand it only if it does not explain "
           "how a backend is added.",
           "Document the three defining traits FIRST -- `Provider`, `Protocol`, and "
           "`Decoder`. Anyone adding a backend implements them and today gets no "
           "contract text at all. State call ordering, error semantics, and who owns "
           "SSE framing. Then `CompletionRequest`, `Capabilities`, `ProviderError`, "
           "`ProviderRouter`, `HttpProvider`, `ProviderKind`, `EventStream`.\n\n"
           "docs/architecture/providers.md documents this same surface -- read it and "
           "stay consistent with it."),
    "Q8": (["hya-mcp"],
           "`lib.rs` has NO `//!` at all. Write one: what MCP is here, how servers are "
           "configured and prepared, and how their tools reach the model namespaced.",
           "Only `McpManager` is documented today. Cover `McpServerConfig`, `prepare`, "
           "`McpClient`, `McpError`, `PreparedMcpServer`, `McpStatus`, `McpTool`, "
           "`namespaced_tool_name`, and the entire 6-item `protocol.rs` wire module "
           "(JsonRpcRequest/Response/Error, ToolInfo, ToolsListResult, ToolCallResult)."),
    "Q12": (["hya-updater", "hya-e2e", "hya-ts"],
            "`hya-updater`'s `//!` is the strongest in the workspace -- leave it. "
            "`hya-e2e`'s is solid -- leave it. **`hya-ts`'s `//!` leads with the crate "
            "name and restates it, and `main.rs` has none** -- rewrite the lib one to "
            "explain the launcher-shim role (resolve and spawn the Bun TUI frontend "
            "against a hya backend) and add one to `main.rs`.",
            "`hya-updater`: `read_floor` (`journal.rs`) is security-adjacent -- it is "
            "the anti-rollback floor, so say what a wrong value permits.\n\n"
            "`hya-e2e`: `ToolCallStep`, `tool_step`, `tools_step` are what a test "
            "author reaches for first.\n\n"
            "`hya-ts`: `Cli` has ~10 undocumented public fields; `BunCommand` and "
            "`invocation_name` also need text."),
    "Q9": (["hya-bundle"],
           "The crate-level `//!` is good; leave it unless it is wrong.",
           "Document `prepare_package` and `stage_package` FIRST -- they are the two "
           "entry points an external caller hits, and `stage_package` is ~150 lines of "
           "staging and cleanup with non-obvious failure semantics (say what is left on "
           "disk when it fails). Then `PreparedBundle`, `PreparedCatalog`, "
           "`PreparedAgent`, `BundleSource`, `PackageInspection`, `PackageFormat`, "
           "`cleanup_orphaned_staging`.\n\n"
           "docs/agent-bundle-authoring.md documents this same format -- read it and "
           "stay consistent with it."),
    "Q11": (["hya-sdk", "hya-client"],
            "`hya-sdk`'s `//!` is one line and does not relate the crate to "
            "`hya-server`/`hya-native` -- expand it. `hya-client`'s `//!` has a stale "
            "consumer clause -- rewrite it.",
            "**`hya-sdk/src/reducer.rs` has a STALE module doc**: it calls `apply` a "
            "'no-op skeleton' and points at a 'W2 deliverable', but `apply` at "
            "`reducer.rs:264` has a full match body. Correct it -- do not merely add to "
            "it.\n\n"
            "For `hya-client`, document `Client`, `ClientError`, `Client::new`, "
            "`create_session`, `prompt`, and `events` -- `events` needs its error and "
            "stream-termination semantics stated.\n\n"
            "NOTE: `reducer.rs`, `store.rs`, and `types.rs` have unrelated uncommitted "
            "edits inside their `mod tests` blocks. Leave those alone; only touch doc "
            "comments."),
    "Q7": (["hya-app", "hya-server"],
           "`hya-app`'s `//!` ends with a stale 'Public surface filled in during Phase "
           "1' clause -- rewrite it. `hya-server`'s is thin -- expand it to name the "
           "route groups it serves.",
           "For `hya-app`: `HyaRuntime` and `HyaRuntime::start` first (the process "
           "entry point), then `RuntimeOptions`, `build_session_engine`, "
           "`RuntimeConfig`, `open_store`, `ResolvedConfig`, `spawn_team_supervisor`, "
           "`ModelEntry`, and the `HyaRuntime` accessor set.\n\n"
           "For `hya-server`: `router` is the crate entry point -- document the route "
           "set and the CORS policy it installs. Then `AppState`, `ApiError`, and the "
           "`McpControl` trait, which is a public extension point with an undocumented "
           "contract. docs/architecture/server-client.md documents these same routes -- "
           "stay consistent with it."),
    "Q2": (["hya-core"],
           "The crate-level `//!` exists but ends with a stale clause about work "
           "deferred to 'later phases'. Rewrite that clause; the phases are done.",
           "Document these FIRST -- they are the crate's public seam:\n"
           "1. `SessionEngine` and its `new`, the whole `with_*` builder chain, and all "
           "accessors.\n"
           "2. `EventBus`, `CreateSession`, `AgentSpec`, `CoreError`.\n"
           "3. **Every public extension trait** -- `HookDispatcher`, "
           "`RuntimeCatalogRefresh`, `Summarizer`, `IterationGate`, `IterationExecutor`, "
           "`GoalEvaluator`, `LoopVerifier`, `LoopPlanner`, `RuntimeSourceOwner`. These "
           "are implemented by downstream crates, so document the CONTRACT: call "
           "ordering, error semantics, and what an implementor owns. Restating the name "
           "is a failure here.\n"
           "4. The hooks.rs Input/Outcome family and `RuntimeSource`.\n\n"
           "`engine/mailbox.rs` has a stale module doc -- correct it. This crate is "
           "yours alone; no other writer touches it."),
    "Q4": (["hya-plugin", "hya-plugin-compat", "hya-plugin-example"],
           "`hya-plugin`'s `//!` ends with a stale 'Phase 0 ships the crate skeleton "
           "only' clause -- rewrite it. `hya-plugin-compat` has NO `//!` at all; write "
           "one saying what the two version pins mean and what breaks when they move. "
           "`hya-plugin-example`'s `//!` is aspirational -- correct it to describe the "
           "stub as a stub.",
           "`messages.rs` alone is ~33 undocumented wire types -- this is the ABI an "
           "external plugin author reads first, so it is the priority. Then "
           "`PluginHost`, `PluginClient`, `PluginSpec`, `PluginEntry`, `Manifest`, "
           "`HookName`, `PROTOCOL_VERSION`, `PluginStatus`, `PermissionBridge`, "
           "`Frame`.\n\n"
           "docs/plugin-protocol.md documents this same ABI -- read it and stay "
           "consistent with it. `hya-plugin-compat` is a 3-line file: two constants."),
    "Q6": (["hya-proto"],
           "The crate-level `//!` is strong; leave it unless it is wrong.",
           "Mostly struct fields and enum variants on WIRE TYPES. Document `Event` "
           "first (the enum the crate exists to define), then the `Projection` family, "
           "`Part`, `TokenUsage`, `Role`, then the api.rs request/response structs.\n\n"
           "For a wire field, say what the CONSUMER should do with it, not that it is "
           "'the id'. docs/architecture/event-model.md documents this same enum -- read "
           "it and stay consistent with it."),
}

PREAMBLE = """# Rustdoc {sub} - {crates}

You are writing Rust API documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

## Your batch

Crate(s): {crates}. **{n} undocumented public items.** Do not touch any other
crate -- other writers are working in parallel.

{cratedoc}

{priority}

## Non-negotiable rules

1. **Read the item before documenting it.** A doc comment that restates the item
   name adds nothing. Say what it is FOR, and for a function, its parameters and
   what it returns.
2. **Do not change code.** No signature, visibility, derive, or behaviour changes.
   Only `///`, `//!`, and `#[doc]` comments. If an item looks like it should not be
   public, leave it public and say so in your report.
3. **Do not run `cargo`.** You do not have approval for shell commands and the
   orchestrator runs the lint to verify. Work from the item list below.
4. **Do not add `#![deny(missing_docs)]`** -- the orchestrator adds it once the
   crate verifies at zero.
5. **Do not run `git commit`.**
6. For a `struct field` or `enum variant`, a single clear line is enough. Spend
   your effort on traits, public entry points, and anything security- or
   protocol-related.
7. If two items mean the same thing, do not paste identical text -- say how they
   differ.

## Item list

Every entry below is a `missing_docs` warning from the compiler, so this list is
exhaustive and mechanical. Grouped by file, with line numbers from the current
tree. Line numbers SHIFT as you insert doc comments -- work bottom-up within a
file, or re-find the item by name.

"""

CLOSING = """
## When you are done

Report:

1. Which files you touched and roughly how many items you documented.
2. Any item you could NOT document because its purpose was not inferable from the
   code, named as `file:line - Name`.
3. Any public item that in your judgement should not be public. Do not change it.
4. Any code defect you noticed. Do not fix it.
"""


def main():
    for sub, (crates, cratedoc, priority) in SUB.items():
        entries = []
        for c in crates:
            entries += ITEMS.get(c, [])
        byfile = collections.defaultdict(list)
        for path, ln, kind in entries:
            byfile[path].append((ln, kind))
        chunks = []
        for path in sorted(byfile):
            rows = sorted(byfile[path])
            chunks.append("### `%s` (%d)\n" % (path, len(rows)))
            chunks.append(", ".join("%d:%s" % (ln, k.replace("struct field", "field")
                                               .replace("associated function", "assoc fn")
                                               .replace("variant", "var"))
                                    for ln, k in rows) + "\n")
        body = PREAMBLE.format(sub=sub, crates=", ".join("`%s`" % c for c in crates),
                               n=len(entries), cratedoc=cratedoc, priority=priority)
        out = HERE / ("rustdoc-%s.md" % sub)
        out.write_text(body + "\n".join(chunks) + CLOSING)
        print("%s  crates=%s items=%d bytes=%d" % (out.name, crates, len(entries), out.stat().st_size))


if __name__ == "__main__":
    main()
