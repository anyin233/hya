/**
 * The left Projects sidebar (docs/tui.md "Projects"; Ctrl+P shows and
 * focuses it, or hides and unfocuses it; `/projects-sidebar` toggles
 * visibility alone): one row per Project, the active one marked, a busy
 * marker `●` while a session of it runs a turn, and its session count.
 * Rows are separated by a rule and switch on a mouse click, like the
 * Sessions list (components/Sidebar.tsx). While focused, Up/Down move the
 * highlight and Enter switches; Esc unfocuses without closing it.
 */
import { For, Show } from "solid-js"
import { useApp } from "../app/context"
import { projectSidebarEntries } from "../state/projectsSidebar"
import { truncate } from "../state/format"
import { colors } from "../theme"

export function ProjectsSidebar(props: { width: number; active?: boolean }) {
  const { store, controller } = useApp()
  const inner = () => Math.max(1, props.width - 4)
  const entries = () => projectSidebarEntries(store.state.projects, store.state.activeProjectId)
  const highlighted = () => store.state.projectSidebarHighlight ?? store.state.activeProjectId
  return (
    <box
      width="100%"
      height="100%"
      flexShrink={0}
      flexDirection="column"
      backgroundColor={colors.bg}
    >
      <box
        width="100%"
        flexGrow={1}
        border
        borderColor={props.active ? colors.accent : colors.border}
        title="Projects"
        backgroundColor={colors.panel}
        flexDirection="column"
        paddingX={1}
      >
        <scrollbox width="100%" flexGrow={1}>
          <Show when={entries().some((entry) => entry.row)} fallback={<text width="100%" wrapMode="word" fg={colors.muted}>No projects yet</text>}>
            <For each={entries()}>
              {(entry) => {
                if (entry.separator) return <text width="100%" height={1} wrapMode="none" fg={colors.border}>{"─".repeat(inner())}</text>
                const row = entry.row!
                const on = () => row.id === highlighted()
                return (
                  <text width="100%" height={1} wrapMode="none" onMouseDown={(event) => { if (event.button === 2) { controller.openProjectContext(row.id); return } if (event.button === 0) controller.switchFromSidebar(row.id) }}>
                    <span style={{ fg: colors.accent }}>{on() ? "▸ " : "  "}</span>
                    <span style={{ fg: row.busy ? colors.warning : colors.fg }}>{row.busy ? "● " : "  "}</span>
                    <span style={{ fg: row.active ? colors.accent : colors.fg }}>{truncate(row.name, Math.max(1, inner() - 8))}</span>
                    <span style={{ fg: colors.muted }}>{` (${row.sessionCount})`}</span>
                  </text>
                )
              }}
            </For>
          </Show>
        </scrollbox>
      </box>
    </box>
  )
}
