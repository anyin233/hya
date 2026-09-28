# 0.43.3

## Thinking-effort repair

- Removed the implicit fallback that pinned a model's maximum thinking effort when none was chosen; an unspecified effort now sends no effort field, so the upstream provider default applies — unspecified means default, not disabled.
- Explicit effort selections are still honored as-is, including a default declared in the model catalog.
- The TUI `/effort` screen now shows the efforts available for the current model and saves a per-model effort preference.
- The existing `model#variant` route (for example `model#high`) still selects an effort directly.
- The model catalog now carries per-model effort menu and default metadata.
