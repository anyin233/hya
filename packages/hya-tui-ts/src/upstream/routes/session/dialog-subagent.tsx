import { createMemo, Show } from "solid-js"
import { DialogSelect } from "../../ui/dialog-select"
import { flattenRunTree, type RunTreeResource } from "./subagent-workspace"

export type SubagentPlacement = "tab" | "vertical" | "horizontal"

export function DialogSubagent(props: {
  resource: () => RunTreeResource
  placement: SubagentPlacement
  open: (sessionID: string, placement: SubagentPlacement) => void
  isOpen: (sessionID: string) => boolean
  isFocused: (sessionID: string) => boolean
  retry: () => void
}) {
  const placement = () => props.placement[0]!.toUpperCase() + props.placement.slice(1)
  const options = createMemo(() => {
    const resource = props.resource()
    const rows = resource.tree ? flattenRunTree(resource.tree) : []
    const options = rows.map((row) => {
      const node = row.node
      const sessionID = node.session
      const label =
        row.depth === 0
          ? `Main · ${node.agent ?? "agent"} · ${sessionID ?? "pending"}`
          : [
              "  ".repeat(row.depth) + (node.roster?.handle ?? (sessionID ? "subagent" : "pending")),
              node.roster?.agent_type ?? node.member?.subagent_type,
              node.roster?.status ?? node.member?.status,
              node.roster?.current_task ?? node.member?.description,
            ]
              .filter(Boolean)
              .join(" · ")
      return {
        title: label,
        value: sessionID ?? node.member?.member ?? `pending:${row.depth}`,
        disabled: !row.selectable,
        footer: sessionID ? (props.isFocused(sessionID) ? "focused" : props.isOpen(sessionID) ? "open" : undefined) : undefined,
      }
    })
    return resource.status === "error"
      ? [{ title: "Subagent tree unavailable - press r to retry", value: "subagent-tree-error", disabled: true }, ...options]
      : options
  })
  return (
    <DialogSelect
      title={`Subagent roster - ${placement()}`}
      options={options()}
      retainDisabled
      flat
      filterActivation="slash"
      onSelect={(option) => props.open(option.value, props.placement)}
      emptyView={
        <Show when={props.resource().status === "error"} fallback={<text>Loading subagent roster...</text>}>
          <text>Failed to load subagent roster. Press r to retry.</text>
        </Show>
      }
      actions={[
        {
          command: "pane.open.vertical",
          title: "Vertical",
          disabled: (option) => !!option?.disabled,
          onTrigger: (option) => props.open(option.value, "vertical"),
        },
        {
          command: "pane.open.horizontal",
          title: "Horizontal",
          disabled: (option) => !!option?.disabled,
          onTrigger: (option) => props.open(option.value, "horizontal"),
        },
      ]}
      bindings={[
        { key: "v", cmd: "pane.open.vertical" },
        { key: "s", cmd: "pane.open.horizontal" },
        ...(props.resource().status === "error"
          ? [
              {
                key: "r",
                cmd: () => props.retry(),
              },
            ]
          : []),
      ]}
    />
  )
}
