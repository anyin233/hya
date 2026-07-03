# 0.30.1

Surface live team state in the comms tools and CLI (follow-ups from the 0.30.0 multi-agent verification).

- The `roster` tool now reports each teammate's live `status` (idle/busy/done/failed), scheduling `mode` (transient/resident), and `current_task` — folded from `AgentActivityChanged` in the projection — instead of a fixed `active` placeholder.
- `hya-backend agent list --all` now also lists user-defined agents discovered on disk (`.claude/agents`, `.hya/agents`, `~/.config/hya/agents`, …) with their category, so a user can confirm a markdown agent is picked up from the CLI. The default `agent list` output is unchanged (Compat-parity).
