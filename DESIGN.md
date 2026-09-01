# hya Design System

## 1. Atmosphere & Identity

hya feels like a quiet terminal command center: dense, fast, and focused, with
just enough surface contrast to keep long coding sessions readable. The
signature is borderless tonal layering: panels are separated by subtle surface
shifts and restrained status colors instead of decorative boxes. The shipping
frontend (`packages/hya-tui-ts`) is multi-theme: dozens of bundled light/dark
themes plus a generated `system` theme, with `theme.switch` / `theme.switch_mode`
/ `theme.mode.lock` for picker and light–dark control. The table below documents
the **default dark `hya` theme** token values as a design baseline, not the only
palette.

## 2. Color

### Palette (default dark `hya` theme)

| Role | Token (JSON / `Theme` field) | Example dark | Usage |
|---|---|---|---|
| Surface/main | `background` | `#0A0A0A` | Transcript background |
| Surface/panel | `backgroundPanel` | `#141414` | Header, footer, overlays |
| Surface/element | `backgroundElement` | `#1E1E1E` | Input row |
| Border/default | `border` | `#484848` | Modal and picker borders |
| Border/active | `borderActive` | `#606060` | Focused borders |
| Border/subtle | `borderSubtle` | `#3C3C3C` | Low emphasis separators |
| Text/primary | `text` | `#EEEEEE` | Main content |
| Text/muted | `textMuted` | `#808080` | Hints, metadata |
| Accent/primary | `primary` | `#FAB283` | Selected options |
| Accent/secondary | `secondary` | `#5C9CF5` | User labels |
| Accent/support | `accent` | `#9D7CD8` | Tool names, thinking state |
| Status/success | `success` | `#7FD88F` | Assistant labels, completed tools |
| Status/warning | `warning` | `#F5A742` | Streaming, pending tools |
| Status/error | `error` | `#E06C75` | Rejections, failed tools |
| Status/info | `info` | `#56B6C2` | Informational accents |

Token names are **camelCase** — the same keys as `Theme` in
`packages/hya-tui-ts/src/upstream/theme/index.ts` and as nested keys under
`"theme"` in shipped assets (`theme/assets/*.json`) and custom
`~/.config/hya/themes/*.json` files. There is **no** snake_case→camelCase
normalization; a file using `background_panel` or `text_muted` will not apply
those tokens.

Light themes supply their own values for the same semantic tokens. Prefer
semantic `Theme` fields in render code; avoid hard-coding hex outside theme
assets.

### Rules

- Prefer semantic `Theme` fields in hya-owned render code; raw literals are
  reserved for retained upstream fallback/data assets (for example error
  fallback values, generated logo art, or which-key bootstrap data). Do not add
  new UI-state colors outside semantic theme roles.
- Accent colors carry meaning and must not be decorative filler.
- Overlays keep `backgroundPanel`; input keeps `backgroundElement`.

## 3. Typography

### Scale

| Level | Size | Weight | Line Height | Tracking | Usage |
|---|---|---|---|---|---|
| Terminal/body | Terminal cell | 400 | Terminal default | 0 | Transcript and input |
| Terminal/strong | Terminal cell | 700 | Terminal default | 0 | Product label, selected options |
| Terminal/meta | Terminal cell | 400 | Terminal default | 0 | Footer, hints, metadata |

### Font Stack

- Primary: terminal emulator default monospace.
- Mono: terminal emulator default monospace.
- Serif: not used.

### Rules

- Do not simulate large type in the TUI; hierarchy comes from color and weight.
- Use bold sparingly for current selection, product identity, and critical state.

## 4. Spacing & Layout

### Base Unit

Terminal spacing derives from a single cell.

| Token | Value | Usage |
|---|---|---|
| `cell-1` | 1 terminal column/row | Horizontal transcript gutter, inline spacing |
| `cell-2` | 2 terminal columns/rows | Overlay inset |
| `row-footer` | 1+ rows | Prompt footer meta line (agent, model, variant, usage) |
| `prompt.max_height` | Config (default ~1/3 of terminal height, minimum 6 rows) | Prompt textarea cap |

### Grid

- The main Session screen uses a vertical stack: transcript, prompt composer
  (with footer meta), and dialogs/overlays as needed. There is **no** separate
  status-line chrome for YOLO/think/goal.
- Subagent observation layouts may use tabs and split panes; observation views
  omit the prompt composer.
- Transcript content has 1-column side gutters.
- Overlays sit near the bottom with 2-column side insets.

### Rules

- Avoid nested framed cards; repeated panels are unframed tonal regions.
- Text must fit terminal width through ellipsizing, wrapping, or horizontal input scrolling.

## 5. Components

### Prompt footer meta

- **Structure**: agent, model, optional variant, and usage/context cues on the
  prompt footer (not a top status line).
- **Spacing**: compact separators; fits terminal width via ellipsis.
- **Accessibility**: state is textual, not color-only.

### Transcript

- **User messages** (`UserMessage` in `packages/hya-tui-ts`): **no** role-name
  label. Agent-colored left border (`border=["left"]`), hover highlight on the
  body panel, optional MIME badges for file parts, optional `QUEUED` badge, and
  an optional compaction divider — not “role label + wrapped lines.”
- **Assistant / system / tools**: message body plus compact tool rows; assistant
  footer/meta and revert banner behavior live in the TypeScript session route
  (see [TUI Reference](docs/tui-reference.md) Transcript section).
- **Spacing**: 1-column side gutters, blank line between messages where applicable.
- **States**: user, assistant, system, tool running, tool completed, tool error.
- **Accessibility**: tool rows include text status and elapsed time when available.

### Prompt Composer

- **Structure**: agent/model context plus grapheme-aware editor that soft-wraps by terminal width.
- **Spacing**: height follows `prompt.max_height` (config), with a practical
  minimum of 6 rows; content scrolls inside the textarea when it exceeds the cap.
- **States**: editable while idle; submitted prompts may remain queued while a
  turn runs; disabled only while a permission/question overlay owns admission;
  hidden cursor when overlays are active; absent when a Subagent observation
  view is focused.
- **Accessibility**: cursor remains visible inside the viewport for long or wide Unicode text when the composer is present.

### Overlay Prompt

- **Structure**: titled panel, detail text, options, keyboard hint.
- **Variants**: permission, question, picker.
- **Spacing**: 2-column screen inset, bottom anchored.
- **States**: selected option, free-text prompt, multi-select prompt, cancel/deny.
- **Accessibility**: current selection uses color plus bold and text position.

## 6. Motion & Interaction

### Timing

| Type | Duration | Easing | Usage |
|---|---|---|---|
| Keystroke / stream text | Immediate | N/A | Input and transcript updates |
| UI motion | Short CSS/OpenTUI transitions | Theme default | Fade-ins, spinner frames |

### Rules

- Prefer immediate updates for typing and streaming text.
- Deliberate motion is allowed: fade-in transitions, block spinners, and
  `app.toggle.animations` (static `[⋯]` when animations are off). Do not add
  gratuitous or non-cancellable animation that blocks input.
- Keyboard controls must remain deterministic and discoverable (command palette
  `ctrl+p`, leader `ctrl+x`, footer/prompt hints).
- Preserve scroll state and cursor state during incremental redraws.

## 7. Depth & Surface

### Strategy

Tonal-shift.

Surfaces use progressively lighter (or theme-appropriate) values. Borders are
allowed for modal focus/containment and for semantic accents or separators such
as user-message splits, the prompt boundary, and compaction dividers. Decorative
framed cards are not used. Shadows are not used.
