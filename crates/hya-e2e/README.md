# hya-e2e

Process-level agent E2E harness: real `hya-backend` + scripted FakeLlm.

Maintainer docs live under the project docs tree (not duplicated here):

- [Testing overview](../../docs/testing/README.md)
- [Process E2E harness](../../docs/testing/process-e2e.md)
- [Agent feature matrix](../../docs/testing/agent-matrix.md)
- Machine registry: [`matrix.toml`](matrix.toml)

## Quick run

```sh
# from workspace root
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e -- --test-threads=1
```
