/**
 * Answering a permission or question prompt (state/prompts.ts): send the
 * choice's `RespondInteraction` body, hide the ask at once, and say what was
 * decided. A failed answer shows the ask again.
 */
import type { HyaClient, Interaction } from "../client"
import { respondBody, type PromptChoice } from "../state/prompts"
import type { AppStore } from "../state/store"

function decided(interaction: Interaction, choice: PromptChoice): string {
  const title = interaction.title
  switch (choice.kind) {
    case "allowOnce": return `Allowed once · ${title}`
    case "allowAlways": return `Always allowed · ${title}`
    case "deny": return `Denied · ${title}`
    case "answer": return `Answered · ${choice.answer}`
    case "reject": return "Rejected the question"
    case "other": return "Type the answer in the input · Enter sends it"
  }
}

export async function answerPrompt({ store, client }: { store: AppStore; client: HyaClient }, interaction: Interaction, choice: PromptChoice): Promise<void> {
  const body = respondBody(choice)
  if (!body) {
    store.setStatus(decided(interaction, choice))
    return
  }
  store.resolveInteraction(interaction.id)
  try {
    const result = await client.respondInteraction(interaction.id, body)
    store.setStatus(result.applied === false ? `Already answered elsewhere · ${interaction.title}` : decided(interaction, choice))
  } catch (error) {
    store.unresolveInteraction(interaction.id)
    store.setStatus(`Answer failed: ${String(error)}`)
    await client.listInteractions().then((rows) => store.setInteractions(rows)).catch(() => undefined)
  }
}
