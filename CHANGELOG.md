# 0.33.36

- Fix subagent pane switching after Ctrl+X V (vertical split): splits always place an observation beside Main instead of nesting under the focused observation, so focusMain / roster agent switches keep working.
- Selecting Main in the subagent roster returns focus to the Main pane; Main rows are selectable.
- Do not swallow leader-chord completion keys while an observation pane is focused.
