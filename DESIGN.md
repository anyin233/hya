# yaca TUI Design System

## 1. Atmosphere & Identity

yaca feels like a quiet terminal command center for agent work: dense, readable,
and immediate, with the transcript as the primary object. The signature is an
OpenCode-like borderless console: a dark streaming surface, a persistent right
context rail, a grounded composer, and transcript turns grouped into selectable
message blocks rather than loose log lines.

## 2. Color

### Palette

| Role | Token | Light | Dark | Usage |
| --- | --- | --- | --- | --- |
| Surface/primary | `surface.primary` | `#FFFFFF` | `#0A0A0A` | Main terminal background |
| Surface/sidebar | `surface.sidebar` | `#F7F7F7` | `#141414` | Right context rail |
| Surface/input | `surface.input` | `#F1F1F1` | `#1F1F1F` | Composer background |
| Surface/block | `surface.block` | `#F3F8FB` | `#18303A` | Selected or emphasized message block |
| Text/primary | `text.primary` | `#111111` | `#EEEEEE` | Transcript body and primary labels |
| Text/secondary | `text.secondary` | `#666666` | `#9A9A9A` | Metadata, hints, inactive controls |
| Text/inverted | `text.inverted` | `#FFFFFF` | `#0A0A0A` | Reverse-video selections |
| Accent/primary | `accent.primary` | `#0EA5B7` | `#56B6C2` | Active rail, prompts, command hints |
| Accent/agent | `accent.agent` | `#0D9488` | `#00BCD4` | Agent identity |
| Accent/model | `accent.model` | `#7C3AED` | `#9D7CD8` | Model identity and reasoning effort |
| Status/success | `status.success` | `#16A34A` | `#7FD88F` | Completed work |
| Status/warning | `status.warning` | `#D97706` | `#F5A742` | Running, thinking, yolo |
| Status/error | `status.error` | `#DC2626` | `#E06C75` | Errors and denied actions |

### Rules

- The primary transcript surface stays borderless and dark.
- Use tonal shifts, rails, and spacing before box borders.
- Accent colors must carry meaning: agent, model, effort, status, or focus.

## 3. Typography

### Scale

| Level | Size | Weight | Line Height | Tracking | Usage |
| --- | --- | --- | --- | --- | --- |
| TUI/body | terminal cell | regular | terminal line | 0 | Transcript text |
| TUI/label | terminal cell | bold | terminal line | 0 | Message author, section labels |
| TUI/meta | terminal cell | regular | terminal line | 0 | Footer, context, and composer metadata |
| TUI/status | terminal cell | bold where active | terminal line | 0 | Active state labels |

### Font Stack

- Primary: terminal monospace configured by the user.
- Mono: terminal monospace configured by the user.

### Rules

- Never rely on color alone; every status has text.
- Preserve CJK width behavior by using ratatui text primitives and wrapping.

## 4. Spacing & Layout

### Base Unit

All terminal spacing derives from cell counts.

| Token | Value | Usage |
| --- | --- | --- |
| `cell.1` | 1 column/row | Inline separators and rails |
| `cell.2` | 2 columns/rows | Message block indentation |
| `cell.3` | 3 columns/rows | Composer prompt gutter |
| `cell.4` | 4 columns/rows | Major transcript breathing room |

### Grid

- Wide terminal: transcript plus fixed right sidebar.
- Narrow terminal: transcript uses full width and sidebar is hidden.
- Composer: input body on the first row, metadata/status on the second row.

### Rules

- Preserve prompt visibility at 80 columns.
- Sidebars and transcript blocks must saturate safely on very small terminals.
- The bottom composer is visually attached to the viewport bottom.

## 5. Components

### Transcript Block

- **Structure**: role label, optional left rail, one or more content lines,
  trailing blank row.
- **Variants**: user, assistant, tool, system error, selected/emphasized.
- **Spacing**: one-cell rail, one-cell label gap, one blank row between turns.
- **States**: normal, running, error, selected.
- **Accessibility**: each block includes a role/status label.

### Composer

- **Structure**: input line plus bottom metadata line.
- **Variants**: idle, streaming, yolo, exit armed.
- **Spacing**: three-cell prompt gutter and one-row metadata band.
- **States**: active input, running disabled cursor, scrollback footer.
- **Accessibility**: agent, model, thinking effort, cost, commands, and mode are
  visible as text.

### Context Sidebar

- **Structure**: title, metrics groups, MCP/team/permission sections, worktree
  and version footer when available.
- **Variants**: visible at wide widths, hidden at narrow widths.
- **Spacing**: two-column left padding.
- **States**: idle, streaming, yolo, permission requested.
- **Accessibility**: bullets and labels accompany all color-coded statuses.

## 6. Motion & Interaction

Ratatui renders immediate frames; there is no animation layer yet.

### Rules

- Streaming state uses text and color changes only.
- Selection/focus uses a rail or tonal block, not blinking decoration.
- Keep cursor placement stable as input grows.

## 7. Depth & Surface

### Strategy

Tonal-shift.

Primary transcript content has no box border. Sidebar separation is a tonal
column, not a rule. Composer separation comes from a darker input surface and a
left accent rail. Message blocks use tonal backgrounds only when selected,
emphasized, or status-bearing.
