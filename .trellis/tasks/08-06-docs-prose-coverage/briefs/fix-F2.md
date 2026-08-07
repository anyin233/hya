# Fix batch F2 - tui-reference.md, tui-keybindings.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/tui-reference.md`
- `docs/tui-keybindings.md`

Do not create or edit any other file.

## What the three kinds of finding mean

- **CONTRADICTION** - the document says something the source does not support. The
  new writing introduced it. This is the worst kind: a reader trusts it today.
  Fix by correcting or DELETING the claim. Never leave the wrong text alongside a
  correction.
- **STILL OPEN** - an original gap the previous writer did not really close.
  Usually "thin": the feature is named but a reader still could not use it.
- **CRITIC** - something no gap entry covered, found by a fresh reader.

## Non-negotiable rules

1. **Open the cited source before you change anything.** Every finding names a
   `file:line`. The auditor may itself be wrong - if the source supports the
   current documentation, KEEP it and say so in your report. Do not "fix" correct
   text because a report told you to.
2. Deleting an unsupported claim is a valid and often correct fix. Do not invent
   replacement behaviour to fill the space.
3. Do not weaken precise contract wording into vague prose. Some sentences in
   these documents are asserted verbatim by tests in `crates/hya-bundle/tests/`;
   if you rewrite a sentence that reads like a contract, keep its exact terms.
4. Edit only your files. Other writers are working in parallel.
5. Do not run `git commit`.

## Findings


### `docs/tui-reference.md`

**CONTRADICTION 1**

- The doc claims: Persisted to `<state>/model.json`. Invalid entries toast a warning rather than failing the load.
- Reality: Same defect as the configuration.md copy — the load path swallows errors with `.catch(() => {})` and emits nothing. Both docs were rewritten with the same wrong sentence.
- Source: `packages/hya-tui-ts/src/upstream/context/local.tsx:180-192`

**CONTRADICTION 2**

- The doc claims: Typing `!` at column 0 of an empty prompt in normal mode switches to shell mode.
- Reality: The `!` binding is enabled purely on `store.mode === 'normal'` and `input?.visualCursor.offset === 0` (plus not-disabled and autocomplete-closed). A non-empty prompt with the cursor at offset 0 also switches to shell mode rather than inserting `!`.
- Source: `packages/hya-tui-ts/src/upstream/component/prompt/index.tsx:786-808`

**CONTRADICTION 3**

- The doc claims: Tool-specific titles cover Edit, Read, Glob, Grep, List, WebFetch, WebSearch, Task, external-directory access, repeated failure, and a generic tool call.
- Reality: The enumeration omits `bash`, which has its own branch: icon `#`, title = the tool's `description` field or the literal `Shell command`, body `$ <command>`. Shell approval is the most frequently seen permission prompt, and the list reads as exhaustive.
- Source: `packages/hya-tui-ts/src/upstream/routes/session/permission.tsx:268-284`

**STILL OPEN 1 - Question prompt (multi-question tab strip, multi-select checkboxes, free-text answer)** (`thin`)

- Source: `packages/hya-tui-ts/src/upstream/routes/session/question.tsx:22-125, 350`
- Why it is still open: The section (lines 200-206) says 'Return to select/submit'. For any multi-select question that is false: `selectOption()` calls `toggle(opt.label)` and returns when `multi()` — Return never submits. Submission requires moving to a trailing **Confirm** tab (`tabs = questions().length + 1`, rendered at line 350; the Confirm tab exists for every case except a single non-`multiple` question) and pressing Return there. The doc documents the `[✓]` checkboxes and the '(select all that apply)' hint but never mentions the Confirm tab, so a reader facing a multi-select prompt cannot finish it from what is written.

**STILL OPEN 2 - Model recents and favorites store (state/model.json)** (`contradicted`)

- Source: `packages/hya-tui-ts/src/upstream/context/local.tsx:162-192, 285-345`
- Why it is still open: Line 365 claims 'Invalid entries toast a warning rather than failing the load.' The load path is `readJson(filePath).then(x => { if (!x || typeof x !== 'object') return; if (Array.isArray(value.recent)) … }).catch(() => {})` — a malformed file, or non-array `recent`/`favorite`, is dropped in total silence with no toast anywhere. The 'Model …/… is not valid' warning toast lives in `model.set` / `model.toggleFavorite` and in the agent-configured-model effect, i.e. it fires when you *select* a model the provider catalog does not serve. The rest of the entry (path `<state>/model.json`, f2 / shift+f2 recents, the 'Add a favorite model to use this shortcut' toast) is correct.

**STILL OPEN 3 - Shell mode (`!` at column 0)** (`contradicted`)

- Source: `packages/hya-tui-ts/src/upstream/component/prompt/index.tsx:786-808`
- Why it is still open: Line 397 says shell mode is entered by 'Typing `!` at column 0 of an **empty** prompt in normal mode'. The binding's `enabled` guard is `inputTarget() !== undefined && !props.disabled && store.mode === 'normal' && !auto()?.visible && input?.visualCursor.offset === 0` — there is no emptiness check. With text already in the buffer, moving the cursor to offset 0 and typing `!` silently flips into shell mode instead of inserting the character. The doc's 'empty prompt' qualifier tells the reader the opposite of what happens.

**CRITIC 1 - Custom TUI themes loaded from disk (`<config>/hya/themes/*.json` and `<ancestor>/.hya/themes/*.json`)**

- Source: `/chivier-disk/yanweiye/Projects/yaca/packages/hya-tui-ts/src/upstream/context/theme.tsx lines 36-63 (`themeSource.discover` + `discoverThemes`); precedence merge in /chivier-disk/yanweiye/Projects/yaca/packages/hya-tui-ts/src/upstream/theme/index.ts lines 170-181 (`listThemes`) and validation in `isTheme` lines 194-198`
- Why it matters: A user can add or replace themes without rebuilding, but no doc says the feature exists or where the files go. `discover()` scans `HyaPaths.config` (`$XDG_CONFIG_HOME/hya`, fallback `~/.config/hya`) then walks `process.cwd()` to filesystem root pushing `<dir>/.hya` at each level, reading `<dir>/themes/*.json`. The file basename becomes the theme name, so it appears in `/themes` and is a valid `theme` config value. Two non-obvious behaviors are user-visible: (1) `listThemes()` spreads `customThemes` AFTER `DEFAULT_THEMES`, so a file named `hya.json` silently REPLACES the shipped default theme; (2) inside `discoverThemes` the per-directory loop assigns `result[name]` unconditionally, so the LAST directory scanned wins — meaning a root-most ancestor's `.hya/themes` overrides the one in the current project directory, the opposite of the usual nearest-wins convention. `isTheme` also requires the JSON to be an object with a nested `theme` object key, and a file that fails that check is dropped with no error. docs/tui-reference.md:502 mentions the precedence chain 'defaults < plugin installs < custom files < system' but never defines what a 'custom file' is or where it lives; docs/configuration.md 'Where the TUI stores state' lists `XDG_CONFIG_HOME → …/hya` only as a generic 'Config dir'; and docs/configuration.md:1256-1259 states the launcher applies 'defaults only via resolve({}, …) and does not load a separate on-disk TUI config file', which actively leads a reader to conclude nothing TUI-side is file-configurable.

**CRITIC 2 - `SIGUSR2` hot-reloads themes in a running TUI**

- Source: `/chivier-disk/yanweiye/Projects/yaca/packages/hya-tui-ts/src/upstream/context/theme.tsx lines 44-48 (`subscribeRefresh` registers `process.on("SIGUSR2", refresh)`), wired at lines 247-248 and torn down at 250-255; the `refresh` handler itself is lines 238-246, which re-runs `refreshSystemTheme()` on a delay ladder and then `syncCustomThemes()``
- Why it matters: This is the only way to pick up an edited custom theme file, or to force re-detection of the terminal's light/dark palette, without restarting the session and losing the attached TUI. `kill -USR2 <bun-pid>` is not discoverable from any keybinding, slash command, or doc — the string SIGUSR2 appears nowhere in docs/. Without it a user editing `~/.config/hya/themes/mine.json` has no idea their change can be applied live, and will restart hya instead. Note the reload is debounced through `THEME_REFRESH_DELAYS`, so the custom-theme rescan only fires on the last delay tick — worth stating so users do not assume the signal was ignored.


### `docs/tui-keybindings.md`

**STILL OPEN 1 - TUI keybinding reference — how to actually override a binding (leader key + every bound command)** (`thin`)

- Source: `packages/hya-tui-ts/src/upstream/config/keybind.ts:41-451`
- Why it is still open: The command tables are genuinely complete (I diffed all 154 entries of `CommandMap` against the page — only session.quick_switch.2-8 are collapsed into a documented range, which is fine). But line 6 tells the reader 'Override any binding through TUI config `keybinds`' and the tables' only identifier column is the *command* name (`session.new`, `command.palette.show`, `prompt.editor`). The accepted `keybinds` config keys are the snake_case `Definitions` keys (`session_new`, `command_list`, `editor_open`), with a mixed sub-vocabulary that IS dotted (`dialog.select.prev`, `prompt.autocomplete.prev`, `permission.prompt.fullscreen`). `TuiKeybind.parse` rejects anything else with `Unrecognized keybind(s): …`. Nothing on the page (or anywhere in docs/) maps command → config key, so a reader following this page writes `keybinds: {"session.new": …}` and gets a hard error. The page is a good read-only reference but not usable for the override task it advertises.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
