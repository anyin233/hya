# Why can a terminal Markdown heading have the expected color before it is bold?

OpenTUI applies tree-sitter styling to each block asynchronously. The dark
palette uses the same foreground for plain text and headings, so waiting for
that color can match the unstyled frame even after the model turn ends. Poll
for the heading's foreground and bold attribute on the same cell. Resolve the
text location again on each poll because styling can change block dimensions.
Wait for paragraph and fenced-code styling separately, since each block can
finish at a different time.

GitHub Actions runs 45480282532 and 45450489624 failed at the heading's bold
assertion. On the original checkout the case passed 12 times with four workers
and unrestricted CPU affinity, then failed 7 of 12 times when those four
workers shared two CPUs (`taskset -c 0,1`). Terminal parity runs the compiled
frontend when `packages/hya-tui/dist/app.js` exists.
