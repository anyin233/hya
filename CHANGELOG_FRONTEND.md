# 0.44.23-dev.2

## Terminal title

- Name the terminal tab after the open session and its Project, with a marker for what it needs: `●` a turn is running, `?` a permission or question ask is waiting, `✓`/`✗` a turn ended while the terminal was unfocused (cleared on focus). The terminal's own title is restored on exit.
- The WebUI's browser tab title follows the terminal title.

## Subagent viewer scrolling

- With the Subagents selector focused, PgUp/PgDn scroll the subagent viewer it previews in (the one Enter pins to), and Home/End jump to its top and newest line. The parent's transcript stays put; with the message editor focused, PgUp/PgDn still scroll the parent's transcript.
