# Why did PR #27 fail browser CI while all unit and backend checks passed?

The browser suite still asserted pre-ert context headings, old opaque backgrounds, and fresh-session startup. Those expectations must follow the documented UI: optional context panes, terminal-default backgrounds, and automatic saved-chat restoration. A sidebar right-click opens a menu but does not imply keyboard focus; tests of strict routing should select the pane explicitly.

The disk-inspector install test calls `target/debug/xtask package-bundle`; a clean runner must build xtask explicitly along with hya.

The dock failures were a real regression: empty OpenTUI composer decoration boxes each measured one row. Omit empty decoration sides, clear their recorded heights on removal, and preserve the editor mount. Do not call width-dependent decoration rendering inside the layout's composer-height callback: width itself comes from solved layout bounds and can introduce a reactive measurement cycle. Regression coverage includes idle/active docks, multiline drafts, legacy layout migration, column-boundary dragging, and an actual installed composer decorator.

## Why did the next full CI run still fail the two-client daemon test?

Run 37627865591 passed all 18 original failures (294/295 browser specs passed). A reconnect reset the main view to chat between `waitForText` and a second PID read. The status helper must wait for the command overlay to close, extract from one buffer snapshot per attempt, and reopen status if reconnect restores chat. Poll for the expected successor PID without nested polling helpers that throw before an outer retry can run.
