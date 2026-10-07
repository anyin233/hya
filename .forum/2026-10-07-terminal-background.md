# How does the TUI inherit terminal transparency without popup text bleeding?

**Answer:** `src/theme.ts` exports `terminalBackground = RGBA.defaultBackground()`.
All built-in themes use it for `bg` and `panel`; startup supplies it to the
renderer too. The native color carries ANSI default-background intent and an
opaque fill, so the emulator controls the visible background/transparency while
popups still erase underlying cells. An alpha-zero `transparent` fill alone
would allow lower content through. Foreground, border and mouse-selection colors
remain theme-specific; Light assumes a light terminal background.

Internal background props accept OpenTUI `ColorInput`. Markdown tests verify
default intent; browser theme specs verify buffer background `default` through
preview/cancel/save/restart, narrow layout, user blocks and code fences.
Frontend aggregate is 0.44.9. TUI typecheck and local build pass, 721 units pass
with one opt-in gRPC skip; three browser specs, xtask and bundle tests pass.
Artifacts are under `~/data/hya-rust/tmp/transparent-background`.
