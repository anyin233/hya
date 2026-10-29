# How should an unfocused pane border use color?

Unfocused frames use the shared ThemeColors.border token, which is achromatic (R=G=B) in every built-in theme: hya #535353, light #b6b6b6, contrast #8a8a8a, ember #4d4d4d. The neutral replacements preserve approximate HSL lightness of the previous tinted borders. Focused frames keep the existing focus token. This also updates extension content using the border semantic role and structural gutters, without changing backgrounds or status colors.

Theme invariant/render tests cover neutral values and focus transitions for all built-ins. Shared real-PTY theme coverage cycles all four themes and checks Sessions gaining/losing focus, alongside existing focus-routing checks. Update exact-color assertions in affected screen specs when changing this shared palette. Leave the independent generic OpenTUI probe's fixed example palette alone.
