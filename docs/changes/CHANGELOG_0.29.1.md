# 0.29.1

- Added startup-safe slash command autocomplete in the default TUI, seeded from built-in local slash commands before backend command discovery finishes.
- Refreshed an open slash autocomplete popup when discovered commands arrive without reopening a dismissed popup.
- Kept slash autocomplete selection in the prompt (`/command `) while exact built-in, quit, and discovered commands still execute on Enter.
