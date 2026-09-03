import { createMemo } from "solid-js"
import { agentModelTargetOptions } from "../../hya/agent-models"
import { DialogModel } from "./dialog-model"
import { useSync } from "../context/sync"
import { useDialog } from "../ui/dialog"
import { DialogSelect } from "../ui/dialog-select"

/**
 * Select a catalog Agent before opening the targeted model picker.
 * @returns The all-Agent target dialog.
 */
export function DialogAgentModels() {
  const sync = useSync()
  const dialog = useDialog()
  const options = createMemo(() => agentModelTargetOptions(sync.data.agentModels))

  return (
    <DialogSelect
      title="Agent models"
      options={options()}
      retainDisabled
      skipFilter
      onSelect={(option) => {
        const row = sync.getAgentModel(option.value)
        if (!row || option.disabled || row.configured || !row.settable) return
        dialog.replace(() => <DialogModel agentID={row.agentID} />)
      }}
    />
  )
}
