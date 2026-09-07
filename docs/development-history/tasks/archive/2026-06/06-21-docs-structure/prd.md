# Design project docs structure

## Goal

Create a maintainable `docs/` information architecture for yaca and populate it
with project documentation that explains how to use, configure, understand, and
develop the Rust workspace.

## Requirements

- Add a `docs/` directory with a clear landing page and topic pages inspired by
  comparable terminal coding-agent projects.
- Cover user-facing usage flows: getting started, configuration, CLI/TUI usage,
  and troubleshooting.
- Cover maintainer-facing system structure: crate responsibilities, runtime
  architecture, event model, provider routing, tools/permissions, persistence,
  server/client API, and TUI boundary.
- Write a detailed project-structure guide that maps repository paths and crates
  to responsibilities and data flow.
- Keep documentation focused on yaca itself. Do not document `.trellis/`, the
  Trellis task system, or Trellis workflow as part of the project docs.
- Keep README links valid and use relative links that work in GitHub Markdown.
- Avoid inventing future behavior. Mark planned or currently absent surfaces as
  out of scope instead of documenting them as shipped.

## Acceptance Criteria

- [ ] `docs/README.md` exists and acts as an index with reading paths.
- [ ] `docs/project-structure.md` gives a detailed repo and crate map.
- [ ] Usage docs explain installation/build assumptions, `yaca`, `yaca exec`,
      goal mode, `serve`, and `tail-session`.
- [ ] Architecture docs explain the current event-sourced session engine and
      major crate boundaries with links to source paths.
- [ ] No project docs page describes `.trellis/` or Trellis workflow.
- [ ] Markdown links across `docs/` resolve locally.
- [ ] Relevant Rust source files are read before writing architecture claims.

## Notes

- Reference projects checked: Compat public docs use a broad topic-page model;
  Oh My Pi docs use deeper engineering-topic manuals. yaca should use a smaller
  hybrid structure because the workspace is currently compact but has strong
  crate boundaries.
