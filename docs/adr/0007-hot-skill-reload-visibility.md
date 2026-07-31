# Hot skill reload visibility

Status: accepted

Implemented by the `0.34.5` `RuntimeRegistry`/`TurnBinding` seam. Skill
discovery occurs once while preparing the workdir view for a turn. Both prompt
exposure and the `skill` tool retain the same immutable catalog, and a
logically unchanged discovery does not advance the generation.

Hot skill reload uses next-turn visibility: an in-flight Turn keeps the skill snapshot it started with, while the next admitted Turn resolves the current skill catalog. This preserves deterministic prompts and immutable event-sourced history while still allowing an already-running runtime to pick up skill changes without forcing a new Session.

Considered alternatives: new-session-only visibility would be simpler to reason about but would make runtime reload less useful; immediate visibility would make mid-stream prompts and tool resolution nondeterministic; configurable visibility would add policy surface before hya has evidence that multiple policies are needed.
