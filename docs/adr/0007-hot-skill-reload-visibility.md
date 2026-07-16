# Hot skill reload visibility

Status: accepted

Hot skill reload uses next-turn visibility: an in-flight Turn keeps the skill snapshot it started with, while the next admitted Turn resolves the current skill catalog. This preserves deterministic prompts and immutable event-sourced history while still allowing an already-running runtime to pick up skill changes without forcing a new Session.

Considered alternatives: new-session-only visibility would be simpler to reason about but would make runtime reload less useful; immediate visibility would make mid-stream prompts and tool resolution nondeterministic; configurable visibility would add policy surface before hya has evidence that multiple policies are needed.
