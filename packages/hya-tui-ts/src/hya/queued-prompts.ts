/** Mid-run prompt delivery: send now, abort-and-steer, or queue until idle. */

export type SessionRunState = "idle" | "busy"

export type PromptSubmitIntent = "submit" | "steer" | "queue"

export type PromptDeliveryAction = "send" | "steer" | "queue"

export type QueuedPromptPart = { type: string; [key: string]: unknown }

export type QueuedPrompt = {
  id: string
  sessionID: string
  text: string
  parts: QueuedPromptPart[]
  createdAt: number
}

export type EnqueueQueuedPrompt = {
  sessionID: string
  text: string
  parts?: QueuedPromptPart[]
}

export type QueuedPromptStoreOptions = {
  now?: () => number
  nextId?: () => string
}

/** Choose send/steer/queue from session run state and the user's submit intent. */
export function resolvePromptDelivery(
  runState: SessionRunState,
  intent: PromptSubmitIntent,
): PromptDeliveryAction {
  if (runState === "idle") return "send"
  if (intent === "steer") return "steer"
  return "queue"
}

/** In-memory FIFO of follow-up prompts waiting for an idle session. */
export function createQueuedPromptStore(options: QueuedPromptStoreOptions = {}) {
  const items: QueuedPrompt[] = []
  let paused = false
  let serial = 0
  const now = options.now ?? Date.now
  const nextId =
    options.nextId ??
    (() => {
      serial += 1
      return `q-${now()}-${serial}`
    })

  return {
    list(sessionID: string): QueuedPrompt[] {
      return items.filter((item) => item.sessionID === sessionID)
    },
    enqueue(input: EnqueueQueuedPrompt): QueuedPrompt {
      const item: QueuedPrompt = {
        id: nextId(),
        sessionID: input.sessionID,
        text: input.text,
        parts: input.parts ?? [],
        createdAt: now(),
      }
      items.push(item)
      return item
    },
    remove(id: string): QueuedPrompt | undefined {
      const index = items.findIndex((item) => item.id === id)
      if (index < 0) return undefined
      const [removed] = items.splice(index, 1)
      return removed
    },
    dequeue(sessionID: string): QueuedPrompt | undefined {
      const index = items.findIndex((item) => item.sessionID === sessionID)
      if (index < 0) return undefined
      const [removed] = items.splice(index, 1)
      return removed
    },
    clear(sessionID: string): void {
      for (let index = items.length - 1; index >= 0; index -= 1) {
        if (items[index]?.sessionID === sessionID) items.splice(index, 1)
      }
    },
    pauseDrain(): void {
      paused = true
    },
    resumeDrain(): void {
      paused = false
    },
    isDrainPaused(): boolean {
      return paused
    },
  }
}

export type QueuedPromptStore = ReturnType<typeof createQueuedPromptStore>
