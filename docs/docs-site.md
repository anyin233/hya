# Documentation Website

The documentation website uses [Astro Starlight](https://starlight.astro.build/)
to publish the existing `docs/` Markdown with sidebar navigation, an on-page
table of contents, light/dark themes, Mermaid diagrams, and Pagefind search.
Markdown remains the single source for both GitHub and the website; no generated
copies or frontmatter migration are required. This site is contributor tooling
and is not included in the `hya` release archive.

## Run locally

Use Node.js `22.23.4` or newer (CI uses Node `24`) and npm. From the repository root:

```sh
cd docs/site
npm ci
npm run dev -- --port 4321
```

Open `http://localhost:4321`. Edits to the source Markdown reload automatically.
For the production build, including the full-text search index:

```sh
npm test
npm run check
npm run build
node scripts/check-build.mjs
npm run preview -- --port 4321
```

Search is generated during `build`; use `preview` to test it. Astro's development
server does not build the Pagefind index.

## Content and routes

`docs/site/src/content.config.ts` uses Astro's glob loader and Starlight's schema.
It reads `docs/**/*.md` except `changes/` (historical release notes), `examples/`
(executable bundle manifests), and `site/` (the site toolchain). The first level-one
heading supplies the page title; Starlight renders that heading once. Add new
top-level pages to the sidebar in `astro.config.mjs`; pages under existing
directory-generated groups appear automatically on the next build. Restart the
development server after adding or renaming a page to refresh these groups.

| Source | Website route (before `DOCS_BASE`) |
| --- | --- |
| `docs/README.md` | `/` |
| `docs/getting-started.md` | `/getting-started/` |
| `docs/architecture/runtime.md` | `/architecture/runtime/` |
| `docs/protocol/README.md` | `/protocol/` |
| `docs/protocol/openapi.json` | `/protocol/openapi.json` (raw JSON) |
| `docs/assets/<file>` | `/<file>` (static asset) |

Relative Markdown links, images, and reference definitions are resolved against
their source file and rewritten with the configured base path. Links to files
outside the published pages (including examples and archived notes) point to
their GitHub sources on `main`. External URLs and fragment-only links are kept.
Each page's edit link points to its original Markdown file on GitHub.

## Configuration interface

| Environment variable | Type | Default | Meaning |
| --- | --- | --- | --- |
| `DOCS_SITE` | Absolute URL string | `https://chrono-foundry.github.io` locally | Canonical origin used in generated metadata and sitemap |
| `DOCS_BASE` | URL pathname string beginning with `/` | `/` locally | Mount path; use `/hya` for repository Pages or `/` for a custom-domain root |

These variables must be set for the build as well as preview. For example:

```sh
DOCS_SITE=https://chrono-foundry.github.io DOCS_BASE=/hya npm run build
DOCS_BASE=/hya node scripts/check-build.mjs
DOCS_BASE=/hya npm run preview -- --port 4321
# Open http://localhost:4321/hya/
```

The site is entirely static and adds no backend routes or runtime API. The only
JSON endpoint is the existing generated OpenAPI document described above.

## GitHub Actions and Pages

`.github/workflows/docs.yml` runs on pushes and pull requests touching `docs/`
or that workflow, and supports `workflow_dispatch`. It installs locked npm
dependencies, runs the focused Markdown adaptation tests, checks Astro types,
builds the website and search index, checks local page/asset links, and uploads
the resulting `github-pages` artifact. This gate is independent of Rust and TUI
CI, so documentation changes do not require a local backend build.

Only a successful build on `main` deploys to GitHub Pages, using the
`github-pages` environment and job-scoped `pages: write` / `id-token: write`
permissions. Branch and pull-request builds never deploy. An owner must first
select **Settings → Pages → Build and deployment → Source: GitHub Actions**;
repository administration is required for this one-time setup. The default
public URL is `https://chrono-foundry.github.io/hya/`.

CI derives `DOCS_SITE` from the repository owner and `DOCS_BASE` from the
repository name. Optional repository Actions variables with these same names
override the defaults. For a custom domain, set `DOCS_SITE` to its HTTPS origin
and `DOCS_BASE` to `/`, and configure the domain in Pages settings.

After the workflow exists on the default branch, it can also be run manually:

```sh
gh workflow run docs.yml --ref <branch>
```

Running on `main` also deploys. Review the Actions run for build/deployment
status; the workflow does not publish application releases or tags.
