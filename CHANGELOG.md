# 0.36.18

## Interactive TUI and tool repairs

- Preserve image attachments and their order across live requests and replay for OpenAI Chat, OpenAI Responses, Anthropic, and Google. Keep text-file references working, accept raw JPEG base64, and reject unsupported media explicitly.
- Restore one-press Escape cancellation in the real terminal by separating the interrupt binding cache from the prompt palette. Preserve shell, autocomplete, modal, and observation-pane key ownership.
- Use one builtin/native skill catalog for advertised commands and captured tool execution. Embedded skills no longer claim filesystem directories or sampled files; existing output limits remain unchanged.
- Make the heap-snapshot command write real V8-compatible files in the hya cache with owner-only permissions, and report actual paths or failures instead of undefined success.
- Connect optional configured or PATH-discovered language servers through the existing LSP plane, with all nine operations, capability-aware routing, scoped diagnostics, live connection status, and owned subprocess teardown.
- Correct the current builtin tool coverage inventory to 27 canonical tools and document language-server configuration and diagnostic freshness limits.

## LSP integration contract

`LspProvider::diagnostics` now receives the requesting workdir and authorized target files. External provider implementations must accept that scope. Versioned or pull diagnostics follow the current document version; unversioned push diagnostics use a bounded two-second best-effort collection window rather than a completion guarantee.
