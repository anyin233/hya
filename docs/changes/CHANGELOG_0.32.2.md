# 0.32.2

TUI layout fix release: keeps the Session screen readable on very short terminals.

- **Fixed Session screen prompt overlap.** The prompt composer now receives its minimum visible rows before the transcript viewport, so short terminals clip transcript content instead of hiding prompt affordances.
- **Added short-terminal layout coverage.** Added a ratatui regression test that proves a real transcript line is clipped while the prompt composer hints remain visible.
