# hya-extra/jev-model-router

Optional `Plugin` bundle: a `chat.params` hook that asks
[Jev](https://docs.typesafe.ai) (TypeSafe System One) how difficult a request
is and rewrites the request's model to the model of the matching tier. One
decision is kept per request chain so a conversation stays on one
provider/model and its prompt cache stays warm.

## Prerequisites

- `bun` (>= 1.2.21, for `Bun.YAML`) on `PATH`
- A TypeSafe API key

## Package and install

```sh
cargo run -p xtask -- package-bundle bundles/extra/jev-model-router jev-model-router.hyabundle
hya bundle install jev-model-router.hyabundle
```

Then write `<hya config dir>/bundles/hya-extra%2Fjev-model-router/config.yml`
(see `config.example.yml`). Without a valid config the router passes every
request through unchanged and says why on stderr.

## Files

- `router.ts` — the plugin process (hya plugin protocol v1 over stdio) and its
  pure routing logic.
- `router.test.ts` — unit tests; run `bun test` in this directory.
- `config.example.yml`, this README — documentation.

Only `router.ts` is declared in `bundle.yaml` and packaged. See
[`docs/extra-bundles.md`](../../../docs/extra-bundles.md) for the full config
contract and failure behaviour.
