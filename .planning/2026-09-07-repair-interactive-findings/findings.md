## Accepted repairs

- F1: TUI URL-shaped images now retain bytes and ordering in provider requests and replay. Live Luna identified the red PNG. Text-file references and raw JPEG base64 retain separate valid handling.
- F2: The shared binding lookup cache omitted the interrupt binding group. A distinct prompt.interrupt group restores one physical Escape; clean live streaming ended with finish=cancelled. Packed ESC ESC bytes represent Alt+Escape, not two physical key presses.
- F3: Advertised builtin skills previously were absent from captured execution. A shared catalog now supplies embedded bodies, preserves native precedence, and records filesystem/embedded/virtual origin. Existing5000-character Generic output limits remain unchanged.
- F4: The palette now writes a real V8 snapshot with0600 permissions and an actual path/error. Live output was19,761,055 bytes. Sensitive heap contents were intentionally not retained.
- F5: Runtime composition now connects configured/PATH-discovered stdio servers through LspPlane. Real TypeScript language server5.3.0 passed all nine operations, displayed connected status, reported a newly introduced type error and cleared it after correction.

## Boundaries and review findings

- LSP client capabilities must be negotiated: otherwise call hierarchy is correctly rejected by capability routing even when the server could support it.
- Diagnostic queries carry workdir and authorized targets. Versioned push/pull diagnostics follow document versions; unversioned push uses bounded two-second best-effort collection, not completion guarantees.
- Transport regressions cover workspace scope, URI identity, capabilities, malformed-server process-tree teardown and interrupted-write cleanup.
- SkillCatalogEntry origin and LspProvider::diagnostics scope are explicit Rust API changes; repository callers migrated, external implementations must adapt.
- Terminal delivery must be observed before interpreting abort results. REST partial-text projection alone was not adequate for timing the clean Escape test; final acceptance used visibly streamed terminal markers without source probes.

## Verification and remaining coverage limits

- Final Rust fmt, workspace all-target Clippy and workspace tests passed with1.91.1. Serial process matrix:44 passed. TUI typecheck and104 tests/2829 assertions passed; KV-file and listener-count warnings were nonfatal.
- The rebuilt local hya and hya-backend report0.36.18; actual hya-ts TUI exercised this backend.
- Repair gateway audit:29 requests,29 responses, only gpt-5.6-luna, no rejected off-model calls.
- Canonical tool count is27. Cumulative live successes are25: baseline24 plus repaired LSP. This is not a fresh full-tool run on0.36.18. Existing modern-GPT policy blocks live write/edit exposure and was intentionally preserved; deterministic tests supplement these gaps.
- Desktop clipboard/IDE notifications, production OAuth/update activation, every UI modifier/media format/mouse branch, and live servers for languages other than TypeScript remain outside completed live acceptance. No blanket all-functions-normal claim.
- report.json maps final acceptance to evidence.zip entries. Earlier investigation captures are retained as context, not substituted for final proof. The archive excludes credential values and full heap contents.
