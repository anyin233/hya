# 0.36.44

## Documentation rewritten for the consolidated contract (docs)

- `docs/architecture/server-client.md` is now the v1-only reference: the
  one-contract/two-transport model (HTTP+SSE+WebSocket and gRPC through
  the same router), app state, the 15-service/77-rpc surface map,
  event-driven and projection-read semantics, guidance parity, command
  expansion, the error table, CORS, clients, and the testing story.
- The component map in `AGENTS.md` reflects the new crate landscape:
  `hya-api` (contract crate + gen-api pipeline), `hya-sdk-v1` (SDK for
  new frontends), the v1-oriented `hya-server`/`hya-client` entries, and
  `hya-sdk`/`hya-native` marked legacy (old TUI only, deletion at the
  new-TUI cutover).
- `docs/README.md` gains a protocol-contract reading path; the
  boundary-to-page table routes server/client API changes to both the
  architecture page and the protocol guide; `docs/development.md`
  documents the `gen-api` xtask (vendored protoc, committed output,
  hya.http coverage/collision gates).
