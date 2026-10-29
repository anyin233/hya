# 0.44.23-dev.11

## Pane resize guides

- Show a Unicode divider guide on hover and while dragging: horizontal `─↕─` for height changes, vertical `│` with `↔` for width changes. Guides follow the affected split, including nested groups, without taking keyboard focus or saving layout changes on hover.
- Request resize pointer shapes where the terminal supports OSC 22, restore the pointer when leaving, and clear guides when overlays open or terminal focus is lost.
