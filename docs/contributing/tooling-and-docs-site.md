# Dev Tooling (`xtask`) & Docs Site

Workspace automation lives in `crates/xtask` (dev-only, never shipped in release archives), and the searchable documentation website lives in `docs/site/` (Astro Starlight).

---

## 1. Developer Tasks (`cargo run -p xtask -- ...`)

Run `cargo run -p xtask -- --help` to see all available tasks:

| Command | When to run it & what it does |
| --- | --- |
| `cargo run -p xtask -- gen-api` | **After editing `proto/hya/v2/*.proto`.** Uses vendored `protoc` to regenerate Rust `hya-api` types, `hya-server` gRPC adapters, `docs/protocol/api-reference.md`, `docs/protocol/openapi.json`, and `packages/hya-tui`'s operation catalog + proto copies. Commit all generated files; CI checks for drift. |
| `cargo run -p xtask -- gen-relay` | **After editing `proto/hya/relay/v1/*.proto`.** Regenerates `crates/hya-relay` bindings. |
| `cargo run -p xtask -- gen-plugin` | **After editing plugin hook types in `crates/hya-plugin/src/messages.rs`.** Regenerates TypeScript `wire.gen.ts` guards and syncs shared stdio runtime files across Bun and Claude adapters. |
| `cargo run -p xtask -- boundary-check [--print-findings]` | **When moving code between the core and `bundles/`.** Runs the ADR-0040 core/bundle boundary guard from `crates/xtask/boundary.toml` (also run by `cargo test -p xtask`): no `crates/` crate may depend on a `bundles/` crate, and core identifiers may not name a bundle domain word. `--print-findings` prints the current violations in allowlist form; remove the entries your change fixes, because stale entries fail. |
| `cargo run -p xtask -- matrix-check` | **After editing `crates/hya-e2e` tests or `matrix.toml`.** Verifies 1:1 Track P scenario registration with zero orphan files or numbering holes. |
| `cargo run -p xtask -- package-bundle <src-dir> <out.hyabundle>` | Validates a `schema_version: 2` bundle source directory and writes a deterministic `.hyabundle` archive. |
| `cargo run -p xtask -- package-native-tool-library <bundle-dir> <lib.so\|.dylib> <out.hyabundle>` | Packages a compiled first-party Native ABI v2 dynamic library (`bundles/builtin/*/native`) into its deterministic `.hyabundle` archive. |
| `cargo run -p xtask -- startup-bench [--db <seed.db>]` | Measures cold `hya serve` startup latency and prints the `HYA_STARTUP_TRACE` phase waterfall. |
| `cargo run -p xtask -- lifecycle-bench [--json] [--check crates/xtask/lifecycle-baseline.json]` | Benchmarks 1/16/64-subagent spawn, work, report/archive, and mail revival SQL/event counts offline. |
| `cargo run -p xtask -- release-rehearsal --version <semver> --target <triple> --no-publish` | Full end-to-end local release staging, checksum, installer, and smoke verification for the host target. |

---

## 2. Landing Page (`public/`) & Documentation Website (`docs/site/`)

GitHub Pages serves the static landing page from `public/` at the site root (`/`) and mounts the **[Astro Starlight](https://starlight.astro.build/)** documentation site under `/docs/`. Starlight renders the Markdown files directly from `docs/` with sidebar navigation, dark/light themes, Mermaid diagrams, and Pagefind full-text search—without duplicating Markdown files or requiring frontmatter in `docs/`.

### Preview locally

Requires Node.js `22.23`+ (CI uses Node `24`) and `npm`:

```sh
cd docs/site
npm ci
npm run dev -- --port 4321
```

Open `http://localhost:4321`. Edits to `docs/**/*.md` hot-reload in the browser.

### Build & link verification gate

`.github/workflows/ci.yml` (`docs` job, gated by `ci-gate`) runs these checks on every PR and `main` push touching `public/`, `docs/`, or `proto/hya/v2/` (checking `public/` static integrity first via `.github/scripts/check_web.py`, then assembling `public/` at `/` and Starlight under `/docs/` in `docs/site/dist-pages`), and `.github/workflows/docs.yml` builds and deploys `main` to GitHub Pages:

```sh
python3 .github/scripts/check_web.py
cd docs/site
npm test
npm run check
npm run build
node scripts/check-build.mjs
```

### How routing and sidebar validation work

- `docs/site/src/content.config.ts` loads `docs/**/*.md` (excluding `changes/`, `examples/`, and `site/`), taking each page's `# H1` heading as its title.
- `docs/site/src/lib/remark-docs.mjs` rewrites relative `.md` links, `assets/*` images, and `protocol/openapi.json` to clean site routes (honoring `DOCS_BASE`, e.g. `/hya` on GitHub Pages), and turns links to repo source files or `docs/examples/*` into GitHub `blob/main/...` links.
- **`scripts/check-build.mjs` enforces two strict rules:**
  1. Zero broken internal page or asset links across the entire built site.
  2. **Every rendered `.md` page in `docs/` must appear in the sidebar navigation** (`docs/site/astro.config.mjs`)—if you add a new Markdown file under `docs/`, make sure its group or slug is listed in `astro.config.mjs`!
