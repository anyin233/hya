# Why did the Markdown terminal test fail after the semantic palette change?

Main CI run 40360805648 contains the earlier test fixes and passed 313 cases,
skipped one browser-only case, and failed the Markdown heading bold assertion.
Commit f0ea6591 changed the default heading color to ordinary text color.
Consequently the old color-only readiness poll could accept an unhighlighted
heading, then immediately fail on bold. Replaying the CI terminal bytes shows
`Release plan` with foreground e8edf3 and bold false. The unchanged test also
failed once in ten two-core/four-worker local repetitions at that exact line.

Wait for color and bold together, and relocate the text on each poll rather
than caching a coordinate across asynchronous block redraws. Paragraph and
fenced-code blocks highlight independently; their style assertions need their
own complete predicates. The component tests also check heading boldness in
the default theme and after a theme switch. Keep the style assertions; turn
completion does not imply highlighter completion.

Verification after repair: 20/20 repetitions on the same two-core/four-worker
setup pass (29.9s), as do seven related terminal scenarios (5.2s), 17 renderer/
palette units, 19 WebUI units, and both package typechecks. This is test-only;
the frontend build succeeds and no shipped version bump is required.
