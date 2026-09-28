# 0.43.0

## Per-model protocol override in provider model lists

- A `models:` entry may now set `kind` to override its provider's protocol for that model, so one channel can serve models on different wire protocols behind one id, base URL, credential, and retry policy — for example an OpenAI-compatible gateway that also fronts Claude Messages models. Before, such a channel had to be split into two providers.
- The entry's effective protocol drives request encoding, the reasoning-variant fallback menu, and routing. The provider's own `kind` still drives remote model-list discovery, provider status, and OAuth/session auth.
- A provider whose entries override `kind` is partitioned into one route per effective protocol under the same provider id; a model ref resolves to the route that claims it, and the partitions route themselves through the existing router.
- The accepted labels are exactly the provider `kind` labels (including `openai-compatible` and `openai-completion`); an unknown label fails config load with `provider <id> model <model> has unknown kind <label> (expected one of …)`. String-form entries cannot override `kind` — use the mapping form.
- A config entry's override also applies when the model is in the model cache: unset config fields keep falling back to the cached metadata, and the fallback effort menu follows the entry's effective protocol, not the provider's.
- The Provider View's model-entry writer (`PUT /v1/providers/{id}/models`) preserves a hand-written `kind` key, like every key it does not manage.
- `HttpProvider::kind()` exposes a route's protocol for diagnostics. See [Configuration](docs/configuration.md).
