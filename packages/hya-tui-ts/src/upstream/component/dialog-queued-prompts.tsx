import { useDialog } from "../ui/dialog"
import { DialogSelect } from "../ui/dialog-select"
import { createMemo, createSignal } from "solid-js"
import { Locale } from "../util/locale"
import { useTheme } from "../context/theme"
import { useQueuedPrompts, type QueuedPrompt } from "./prompt/queued"
import { useCommandShortcut } from "../keymap"

function getRelativeTime(timestamp: number): string {
  const now = Date.now()
  const diff = now - timestamp
  const seconds = Math.floor(diff / 1000)
  const minutes = Math.floor(seconds / 60)
  const hours = Math.floor(minutes / 60)
  const days = Math.floor(hours / 24)

  if (seconds < 60) return "just now"
  if (minutes < 60) return `${minutes}m ago`
  if (hours < 24) return `${hours}h ago`
  if (days < 7) return `${days}d ago`
  return Locale.datetime(timestamp)
}

function getPreview(input: string, maxLength: number = 50): string {
  const firstLine = input.split("\n")[0]?.trim() ?? ""
  return Locale.truncate(firstLine, maxLength)
}

export function DialogQueuedPrompts(props: {
  sessionID: string
  onSelect: (entry: QueuedPrompt) => void
}) {
  const dialog = useDialog()
  const queued = useQueuedPrompts()
  const { theme } = useTheme()
  const [toDelete, setToDelete] = createSignal<string>()
  const deleteHint = useCommandShortcut("queued_prompt.delete")

  const options = createMemo(() =>
    queued.list(props.sessionID).map((entry) => {
      const isDeleting = toDelete() === entry.id
      return {
        title: isDeleting ? `Press ${deleteHint()} again to confirm` : getPreview(entry.text),
        bg: isDeleting ? theme.error : undefined,
        value: entry.id,
        description: getRelativeTime(entry.createdAt),
      }
    }),
  )

  return (
    <DialogSelect
      title="Queued prompts"
      options={options()}
      onMove={() => {
        setToDelete(undefined)
      }}
      onSelect={(option) => {
        const entry = queued.remove(option.value)
        if (entry) props.onSelect(entry)
        dialog.clear()
      }}
      actions={[
        {
          command: "queued_prompt.delete",
          title: "delete",
          onTrigger: (option) => {
            if (toDelete() === option.value) {
              queued.remove(option.value)
              setToDelete(undefined)
              return
            }
            setToDelete(option.value)
          },
        },
      ]}
    />
  )
}
