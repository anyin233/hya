# What do TUI colors mean, and how should a new pane choose one?

Use the semantic roles in `packages/hya-tui/src/theme.ts`. `focus` means
selection, keyboard ownership, or the input cursor. `heading` is neutral
structure; `link` is neutral and underlined; `activity` is neutral progress
with an animated glyph or running label. Green `success`, amber `warning`,
and red `error` carry completion, attention, and failure/danger respectively.
`fg`, `muted`, and `border` cover ordinary content, secondary information,
and inactive structure. `selection` is the mouse-selection background.

Do not use focus as generic emphasis. A Box title must set `titleColor`
independently of `borderColor`, or OpenTUI paints the title as if selected.
Markdown headings and assistant identity use heading, not focus. Inactive
current-value dots are neutral; the highlighted choice carries focus.

Code uses a separate small grammar palette: purple keywords, warm-gold
literals/inline code, neutral names/types/operators, and gray italic comments.
Diff additions/removals are green/red; location headers are neutral. Neither
grammar nor diff headers consume navigation blue.

Extensions accept the same foreground tokens. Their old `accent` token is a
compatibility alias for focus; new first-party code uses explicit roles. The
basic TUI component bundle follows these rules too, so this change affects
both frontend and backend shipped assets.

## What can break verification in an isolated worktree?

The extension OS sandbox allows its SDK directory, not arbitrary symlink
targets. An SDK node_modules symlink outside that directory caused confined
hosts to fail to resolve QuickJS. Hard-linking the existing dependencies into
the worktree (outside the filesystem overlay) verified the normal layout
without duplicating dependency data or loosening confinement.

The compiled frontend also expects dist/ beside its package resources.
Symlinking the whole dist directory to another disk changes import.meta.dir
and resource/dependency resolution. Keep its small runtime copy in the normal
package layout; bulky Rust caches and PTY artifacts remain under ~/data.
