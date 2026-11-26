# hya-e2e

Process-level agent E2E harness: real `hya` + scripted FakeLlm.

Maintainer docs live under the project docs tree (not duplicated here):

- [Testing & Verification Guide](../../docs/contributing/testing.md)
- Machine registry: [`matrix.toml`](matrix.toml)

## Quick run

```sh
# from workspace root
cargo build -p hya-backend --bin hya
cargo test -p hya-e2e -- --test-threads=1
```
