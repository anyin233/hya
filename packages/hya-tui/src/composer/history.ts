/**
 * Input history for the composer: every submitted input (prompts, `!shell`
 * commands, slash commands), oldest first, kept for the life of the TUI
 * process (not persisted). Up on the input's first line steps to older
 * entries; Down on its last line steps back and, past the newest entry,
 * restores the draft that was in the input when navigation began.
 */
export class InputHistory {
  private readonly items: string[] = []
  /** Index into `items` while navigating; `undefined` when editing the draft. */
  private index: number | undefined
  private draft = ""

  constructor(private readonly limit = 200) {}

  get entries(): readonly string[] { return this.items }

  get navigating(): boolean { return this.index !== undefined }

  /** Record a submitted input and leave navigation. Empty and repeated inputs are skipped. */
  push(text: string): void {
    this.reset()
    if (!text.trim() || this.items.at(-1) === text) return
    this.items.push(text)
    if (this.items.length > this.limit) this.items.splice(0, this.items.length - this.limit)
  }

  /** The next older entry, or `undefined` when there is none. `current` is saved as the draft on the first step. */
  previous(current: string): string | undefined {
    if (this.index === undefined) {
      if (!this.items.length) return undefined
      this.draft = current
      this.index = this.items.length - 1
      return this.items[this.index]
    }
    if (this.index === 0) return undefined
    this.index -= 1
    return this.items[this.index]
  }

  /** The next newer entry, the draft after the newest one, or `undefined` when not navigating. */
  next(): string | undefined {
    if (this.index === undefined) return undefined
    if (this.index < this.items.length - 1) {
      this.index += 1
      return this.items[this.index]
    }
    const draft = this.draft
    this.reset()
    return draft
  }

  /** Stop navigating (the input was edited or submitted). */
  reset(): void {
    this.index = undefined
    this.draft = ""
  }
}
