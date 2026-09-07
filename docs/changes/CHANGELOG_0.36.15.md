# 0.36.15

## Escape cancellation

- Restore one-press Escape cancellation by separating interrupt bindings from the prompt palette cache.
- Avoid duplicate in-flight aborts and preserve shell, autocomplete, modal, and observation-pane Escape ownership.
