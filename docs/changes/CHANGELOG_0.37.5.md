# 0.37.5

## OpenTUI frontend for the v1 backend

Added `packages/hya-tui`, a Bun and OpenTUI terminal frontend that connects to
an existing `hya-backend serve` process. It presents sessions, projected
messages, live turn updates, model and Workflow catalogs, and pending
permission/question interactions. A `/api` command view lists the generated
`hya.v1` HTTP operations and can send JSON requests to the remaining endpoints.

The frontend keeps the backend as the owner of session state. It uses the
`/v1` HTTP/JSON and SSE contract with directory scoping and reconnects its
event stream after a disconnect. It does not require a live provider; the
backend's offline model works for initial setup.

The Claude adapter's package pin test now checks the actual adapter package
version, which also keeps the workspace Clippy gate clean on Rust 1.91.
Workflow documentation tests now read the committed guide, so a fresh clone
does not depend on an ignored local wiki checkout.
The v1 HTTP binding now decodes `page.cursor` and `page.limit` query keys into
the nested page request used by the Rust SDK and the new frontend.
