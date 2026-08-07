# Fix batch G3 - tui-reference.md, tui-keybindings.md, tui.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/tui-reference.md`
- `docs/tui-keybindings.md`
- `docs/architecture/tui.md`

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

- The doc claims: In "Copy and clipboard": mouse-up AND right-click copy the selection, Ctrl+C over a selection copies instead of exiting, Escape clears the selection — and "All of the above are disabled when `HYA_DISABLE_COPY_ON_SELECT` is set, and are **always** disabled on `win32`."
- Reality: Inverted for three of the four items. `onMouseUp` auto-copy runs only when `!HyaFlag.disableCopyOnSelect`. Right-click copy (`onMouseDown`, `evt.button !== MouseButton.RIGHT` guard) and the Ctrl+C / Escape selection intercept both run only when `HyaFlag.disableCopyOnSelect` IS true — the code comment says "Let selection copy/dismiss win ahead of normal bindings when explicit copy is required." So setting the variable (or running on win32, where `disableCopyOnSelect` is forced true) turns those three ON, not off. The doc tells win32 users that all TUI copying is unavailable when in fact win32 is exactly the explicit-copy mode.
- Source: `packages/hya-tui-ts/src/upstream/app.tsx:399-406 and :915-927; src/hya/platform.ts:41`

**CONTRADICTION 2**

- The doc claims: Themes section: "Valid files must be a JSON object with a nested `theme` object key; **invalid files are dropped silently**."
- Reality: Only true for files that parse as JSON but fail `isTheme`. `discoverThemes` calls `JSON.parse(await readFile(...))` inside the scan loop with no try/catch, so one malformed `.json` file rejects the whole `discover()` promise; `syncCustomThemes()` then hits `.catch(() => setStore("active", "hya"))`, which drops **every** custom theme (setCustomThemes is never called) and force-resets the active theme to `hya`. That is a user-visible failure mode the doc says does not happen.
- Source: `packages/hya-tui-ts/src/upstream/context/theme.tsx:51-63 and :129-141`

**CRITIC 1 - Pasting a local file path or `file://` URL into the prompt attaches that file as a media part (the terminal drag-and-drop flow), for a fixed set of image/PDF types**

- Source: `packages/hya-tui-ts/src/upstream/component/prompt/index.tsx:70-79 (`pastedFilepath`: strips surrounding quotes, converts `file://` URLs, unescapes shell-escaped spaces on non-win32), index.tsx:1119-1140 (`pasteInputText`: skips http(s) URLs, otherwise tries `readLocalAttachment` before falling back to plain text), packages/hya-tui-ts/src/upstream/component/prompt/local-attachment.ts:25-47 (extension→MIME map and the binary/text split)`
- Why it matters: This is how a user attaches an existing file on disk to a prompt — the behavior you get when you drag a file onto the terminal, or paste a path. docs/tui-reference.md:436 documents only the clipboard branch ("inserts clipboard text or attaches a clipboard image as a `clipboard` file part"), so the on-disk path branch is invisible in docs. The undocumented specifics matter in practice: only `.avif`, `.gif`, `.jpeg`, `.jpg`, `.pdf`, `.png`, `.svg`, `.webp` attach — every other path, including `.txt`/`.md`/source files, is silently pasted as literal text rather than attached, which reads as a bug; `.svg` is special-cased to be inserted as inline text behind an `[SVG: <name>]` placeholder instead of a binary attachment; `file://` URLs work but `http(s)://` URLs are deliberately never fetched; and quoting/backslash-escaping applied by the terminal on drop is stripped, which is why a path with spaces works at all.


### `docs/tui-keybindings.md`

**CONTRADICTION 1**

- The doc claims: Header: "Override bindings with TUI config `keybinds` using the config keys in [Overriding keybinds]", followed by a whole section with a YAML example (`keybinds:\n  session_new: "<leader>N"`) and a 170-row config-key→command table presented as user-writable configuration.
- Reality: There is no load path for a TUI config in the shipped product. `main.tsx` calls `resolve({}, { terminalSuspend: … })` — an empty object, defaults only — and `crates/hya-app/src/config.rs` has no `tui` / `keybinds` key at all, so nothing a user writes in `config.yaml` reaches `TuiKeybind.parse`. docs/configuration.md states this honestly ("applies **defaults only** ... does **not** load a separate on-disk TUI config file"), but tui-keybindings.md never says it and reads as a working how-to. A reader following it gets either silent no-op or a backend config-schema error.
- Source: `packages/hya-tui-ts/src/main.tsx:55; crates/hya-app/src/config.rs (no `tui` key)`


### `docs/architecture/tui.md`

**CONTRADICTION 1**

- The doc claims: "Frontend Plugin Host ... static host: **starts all builtin plugins in parallel**", then lists eleven builtin ids including `which-key` as item 10, with no note that any of them is inactive.
- Reality: `createStaticPluginHost` starts `createBuiltinPlugins().filter((plugin) => plugin.enabled !== false)`, and the which-key plugin ships `enabled: false`. Ten of the eleven start; the which-key panel does not load. docs/tui-keybindings.md calls this out explicitly ("**Default off**"), so the two pages disagree about whether the panel is live.
- Source: `packages/hya-tui-ts/src/hya/static-host.ts:31; src/upstream/feature-plugins/system/which-key.tsx:602-605`

**CONTRADICTION 2**

- The doc claims: "Startup navigation sequence" step 2: "**`--session` without `--fork`** navigates to that session immediately", presented as an independent step after step 1's invalid-model toast.
- Reality: Steps 1 and 2 run inside the same `batch()` callback, and the invalid-model branch is `return toast.show(...)` — it returns from that callback. So `hya --model badformat --session <ID>` shows the warning toast and never performs the `--session` navigation; the user lands on Home instead. The documented sequence implies the steps are independent.
- Source: `packages/hya-tui-ts/src/upstream/app.tsx:455-474`

**CRITIC 1 - which-key builtin TUI plugin (loaded vs. shipped disabled)**

- Source: `/chivier-disk/yanweiye/Projects/yaca/packages/hya-tui-ts/src/hya/static-host.ts:30 (`createBuiltinPlugins().filter((plugin) => plugin.enabled !== false)`) and /chivier-disk/yanweiye/Projects/yaca/packages/hya-tui-ts/src/upstream/feature-plugins/system/which-key.tsx:604 (`enabled: false`)`
- Why it matters: `docs/architecture/tui.md:307-324` says the static host "starts **all** builtin plugins in parallel" and lists eleven ids including `which-key`, with no mention of the enabled filter. `docs/tui-keybindings.md:506-509` says "**Default off:** the shipped which-key plugin sets `enabled: false` and is filtered out of the static builtin host … the panel does not load unless that plugin is re-enabled", and `docs/FOLLOWUPS.md:74-78` records the same as a confirmed defect. Only ten of the eleven declared builtins actually start.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
