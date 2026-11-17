# How is the documentation site built without duplicating Markdown?

The standalone toolchain lives under `docs/site`. A wrapped Astro glob loader
reads the canonical `docs/**/*.md` files, infers titles from their first H1, and
sets edit URLs to their real sources. Exclude `changes/`, `examples/`, and `site/`:
example `.hya.md` files are executable manifests, not prose pages.

The remark adapter resolves links against each source Markdown file and prefixes
published routes/assets with `DOCS_BASE`. Other repository files link to GitHub.
The production build check exercises `/hya` paths as well as local links, assets,
raw OpenAPI JSON, and the Pagefind index. Search requires build + preview.

Starlight 0.43 rejects the older `{ label, autogenerate }` sidebar shape. More
importantly, its built-in autogenerate filters by `src/content/docs` file paths
and silently produces empty groups for external loaders. Use `documentLinks`
to generate explicit slug entries from the source tree; the production check
asserts that every rendered page is reachable from the homepage navigation. Astro 7 uses the explicit
`@astrojs/markdown-remark` unified processor for remark plugins. Do not assume
older Astro/Starlight examples match current APIs.

# What deployment and preview constraints were observed?

The GitHub account has push access but no repository administration. Reading or
creating a Pages site returned 404, so a repository owner must enable GitHub
Actions as the Pages source. The workflow builds branch/PR changes and only
deploys successful main builds. No release version bump is needed: the site is
not shipped in the application archive.

The shared T3 browser was blocked by host AppArmor; local Google Chrome can run
Playwright with `--no-sandbox` for optional website QA. Avoid changing host
security settings merely to preview documentation.

# Why did the relay sequence diagram fail in the website?

Its sequenceDiagram used flowchart arrow syntax `Bridge<-->Host`. Mermaid
sequence diagrams require `Bridge<<->>Host` for bidirectional solid arrowheads.
The source was corrected so both the website and GitHub can render it.
