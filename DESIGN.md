# hya Design System

## 1. Atmosphere & Identity

hya feels like a quiet terminal command center: dense, fast, and focused, with
just enough surface contrast to keep long coding sessions readable. The
signature is borderless tonal layering: panels are separated by subtle dark
surface shifts and restrained status colors instead of decorative boxes.

## 2. Color

### Palette

| Role | Token | Light | Dark | Usage |
|---|---|---|---|---|
| Surface/main | `background` | N/A | `#0A0A0A` | Transcript background |
| Surface/panel | `background_panel` | N/A | `#141414` | Header, footer, overlays |
| Surface/element | `background_element` | N/A | `#1E1E1E` | Input row |
| Border/default | `border` | N/A | `#484848` | Modal and picker borders |
| Border/active | `border_active` | N/A | `#606060` | Focused borders |
| Border/subtle | `border_subtle` | N/A | `#3C3C3C` | Low emphasis separators |
| Text/primary | `text` | N/A | `#EEEEEE` | Main content |
| Text/muted | `text_muted` | N/A | `#808080` | Hints, metadata |
| Accent/primary | `primary` | N/A | `#FAB283` | Selected options |
| Accent/secondary | `secondary` | N/A | `#5C9CF5` | User labels |
| Accent/support | `accent` | N/A | `#9D7CD8` | Tool names, thinking state |
| Status/success | `success` | N/A | `#7FD88F` | Assistant labels, completed tools |
| Status/warning | `warning` | N/A | `#F5A742` | Streaming, pending tools |
| Status/error | `error` | N/A | `#E06C75` | Rejections, failed tools, YOLO warning |
| Status/info | `info` | N/A | `#56B6C2` | Informational accents |

### Rules

- Use only semantic theme fields from `Theme`; no raw `Color::Rgb` in render code.
- Accent colors carry meaning and must not be decorative filler.
- Overlays keep `background_panel`; input keeps `background_element`.

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
| `row-status` | 1 row | Status line |
| `row-input` | 6-11 rows | Width-aware expanding prompt region |
| `row-footer` | 1 row | Keyboard hint footer |

### Grid

- The main Session screen uses the vertical stack: status, transcript, Prompt composer, footer.
- Subagent observation layouts may use tabs and split panes; observation views omit the Prompt composer.
- Transcript content has 1-column side gutters.
- Overlays sit near the bottom with 2-column side insets.

### Rules

- Avoid nested framed cards; repeated panels are unframed tonal regions.
- Text must fit terminal width through ellipsizing, wrapping, or horizontal input scrolling.

## 5. Components

### Status Line

- **Structure**: product label, session label, running state, optional YOLO/think/goal state.
- **Spacing**: inline `cell-1` and compact separators.
- **States**: idle, streaming, YOLO, thinking effort, goal active.
- **Accessibility**: state text is visible, not color-only.

### Transcript

- **Structure**: role label followed by wrapped message lines and compact tool rows.
- **Spacing**: 1-column side gutters, blank line between messages.
- **States**: user, assistant, system, tool running, tool completed, tool error.
- **Accessibility**: tool rows include text status and elapsed time when available.

### Prompt Composer

- **Structure**: agent/model prefix plus grapheme-aware editor that soft-wraps by terminal width.
- **Spacing**: `row-input` height with no border; text grows from 1 to 6 visible rows, then scrolls to keep the cursor row visible.
- **States**: editable, disabled while running, hidden cursor when overlays are active, absent when a Subagent observation view is focused.
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
| Terminal update | Immediate | N/A | Keystrokes, streaming text, selection movement |

### Rules

- Terminal rendering is immediate; do not add animation artifacts.
- Keyboard controls must remain deterministic and discoverable in footer/prompt hints.
- Preserve scroll state and cursor state during incremental redraws.

## 7. Depth & Surface

### Strategy

Tonal-shift.

Surfaces use progressively lighter dark values. Borders are allowed only for modal
overlays where focus and containment must be explicit. Shadows are not used.
