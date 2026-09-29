# 0.43.6

## Anthropic prompt caching

- Anthropic routes now mark up to three `cache_control: {"type": "ephemeral"}` breakpoints per request: the last system block, the last tool definition, and the last content block of the final message. Repeated tool-loop requests reuse the cached prefix instead of paying for the full prompt every step.
- New per-provider switch `providers.<id>.prompt_cache` (boolean; default `true` for `kind: anthropic`, ignored for other kinds). Set `prompt_cache: false` for an Anthropic-compatible gateway that rejects `cache_control`.

```yaml
providers:
  my-gateway:
    kind: anthropic
    base_url: https://gateway.example/v1
    prompt_cache: false
```
