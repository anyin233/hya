# Session lifecycle display

The sessions picker protects team members from deletion, and the transcript
marks revived episodes separately from context compaction.

Open `/sessions`, highlight a root session, and press Ctrl+D to confirm deletion.
For a row whose `SessionInfo.parent` is set, Ctrl+D stays in the list and shows
“Archive the subagent or delete the root”; it sends no delete request. For example,
a child beneath a root can be opened or renamed, while deletion targets its root.

A system message beginning with `HYA_COMPACTED_CONTEXT\nPRIOR HANDOFF (generation N)\n`
starts a revived episode. The transcript shows the plain boundary
“resumed from handoff (generation N)” and retains the handoff text. `N` is a
nonnegative integer. Other compacted-context summaries retain the context
compacted divider; no API or event contract changes.

Picker actions may supply `disabled?: (row: PickerRow) => string | undefined`.
A returned reason keeps the picker in list mode and becomes its hint, before
rename or confirmation can begin; `undefined` permits the action.
