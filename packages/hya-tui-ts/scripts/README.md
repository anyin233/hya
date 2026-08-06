# hya-tui-ts scripts

## `prune-sdk-server.ts`

Release and install preparation for a **runtime directory** that already has
`node_modules/@opencode-ai/sdk` installed (production install of this package).

### Invocation

```sh
bun packages/hya-tui-ts/scripts/prune-sdk-server.ts <runtime-dir>
```

- **`argv[2]`** (`Bun.argv[2]`) is the runtime directory root (the prepared
  `hya-tui-ts` tree, not the monorepo root).
- Missing argument → throws `runtime directory is required`.

### What it does

1. Opens `<runtime>/node_modules/@opencode-ai/sdk/package.json`.
2. Requires an export map that includes `./v2/client`.
3. Rewrites exports so:
   - `./v2` resolves to the same target as `./v2/client`
   - top-level `.`, `./server`, and `./v2/server` exports are removed
4. Deletes server/process (and related) dist files under `dist/`:
   `index.js`, `index.d.ts`, `server.js`, `server.d.ts`, `process.js`,
   `process.d.ts`, `v2/index.js`, `v2/index.d.ts`, `v2/server.js`,
   `v2/server.d.ts`.
5. Spawns a probe in that runtime:
   `import { createOpencodeClient } from "@opencode-ai/sdk/v2"` and asserts it is
   a function. Failure throws `pruned SDK client import failed: …`.

After prune, the frontend can import the **v2 client only**; server/process
entrypoints from the SDK package are not retained in the shipped runtime.

### Callers

| Caller | Role |
| --- | --- |
| `install.sh` | After placing the prepared TypeScript runtime under `lib/hya/hya-tui-ts` |
| `.github/workflows/release.yml` | After copying package sources into the release package tree |

### Tests

Guarded by **`test/runtime-boundary.test.ts`**: installs a temporary production
runtime, runs this script, verifies `@opencode-ai/sdk/v2` imports, and builds
`src/main.tsx` with the pruned tree.

Also related: **`test/boundary.test.ts`** pins legal/dependency boundary for the
source package (not the prune step itself).

## `generate-logo-art.py`

Offline artwork generator for the home logo and session epilogue terminal art.

- **Docstring** at the top of the script is the authoritative usage guide
  (Pillow / NumPy / SciPy via `uv run --with …`).
- Default source: `docs/assets/hya-icon-8bit.png` (8-bit wordmark).
- Emits TypeScript data modules under `src/upstream/component/logo-art.data.ts`
  and `src/upstream/util/epilogue-art.data.ts` (quadrant-block glyphs).
- Design notes: [docs/research/terminal-icon-rendering.md](../../../docs/research/terminal-icon-rendering.md).

Not invoked by install/release by default; re-run when the wordmark asset
changes.
