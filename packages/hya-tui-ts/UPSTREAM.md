# Upstream provenance

- Repository: https://github.com/anomalyco/opencode
- Version: 1.17.9
- Commit: `cf31029350820c6bfc0fbd0e052a79a067ee6116`
- Upstream package: `packages/tui`

## Imported boundary

`src/upstream` is derived from the frontend source and theme assets in
`packages/tui/src`. The retained runtime uses the upstream SolidJS/OpenTUI TUI,
the `@opencode-ai/sdk/v2` HTTP/SSE client, and static built-in TUI components.
The audio files under `src/upstream/assets/audio` come from
`packages/ui/src/assets/audio` and are used by that frontend.

hya replaces the small `@opencode-ai/core` path, flag, version, lock, glob, and
executable-lookup uses with hya-owned or platform implementations. It also uses
local copies of the retained audio assets. The imported frontend has been
rebranded for hya: upstream branding and artwork are replaced with hya project
branding, while the upstream copyright is retained per `LICENSE`.

## Excluded boundary

This package excludes OpenCode backend, server, provider runtime, worker/RPC,
updater, Console/organization, web/desktop, and external dynamic plugin loader
or plugin-manager modules. It is a frontend only and connects to hya through
the pinned SDK compatibility protocol.

## Rebranding

Rebranding of the imported frontend is complete. Upstream product branding and
artwork have been replaced with hya project branding; the upstream copyright
and permission notices are retained in `LICENSE`. See `NOTICE` for details.

The logo artwork shown on the home screen and in the session epilogue is
generated from the 8-bit Hya wordmark (`docs/assets/hya-icon-8bit.png`) by
`scripts/generate-logo-art.py` into `src/upstream/component/logo-art.data.ts`
and `src/upstream/util/epilogue-art.data.ts`. The classic full sticker remains
at `docs/assets/hya-icon.png` for docs/README use.
