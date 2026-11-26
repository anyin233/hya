# 5-Minute Quickstart

This guide walks you from a fresh install to your first real task in Hya. If you haven't installed `hya` yet, follow [Installation](installation.md) first.

---

## 1. Open Hya in a project folder

`cd` into any code folder on your computer (or make a scratch folder to try it out) and run `hya`:

```sh
cd ~/my-project
hya
```

Three things happen right away:

1. Hya starts a small **background server** so your chats stay safe even if you close the terminal.
2. The **Terminal UI (TUI)** opens in front of you.
3. The **WebUI** starts at `http://127.0.0.1:3250`—open that address in your browser if you prefer clicking around in a browser tab.

Try typing `hello` and pressing **Enter**. Because you haven't added an API key yet, Hya replies using its built-in offline echo model (`hya/offline`). That confirms everything is working.

---

## 2. Connect an AI model

You can add an API key without ever leaving Hya:

1. Type `/key` and press **Enter** to open the **Provider View**.
2. Press **`a`** to add a provider:
   - **Name:** pick any short nickname, like `anthropic` or `openai`.
   - **Protocol:** pick `anthropic`, `openai-response` (for OpenAI), `openai` (for OpenAI-compatible servers like OpenRouter, DeepSeek, or Ollama/vLLM), or `google` (for Gemini).
   - **Base URL:** e.g. `https://api.anthropic.com/v1` or `https://api.openai.com/v1`.
   - **API key:** paste your key and press **Enter** (your key is hidden as bullets).
3. Hya fetches the model list automatically and opens a picker so you can select your model. Press **Esc** twice to return to your chat.

Prefer setting it up from the terminal or signing in with a subscription instead? Quit Hya (`Ctrl+C` twice) and run one of these:

```sh
# Guided terminal wizard
hya provider add

# Or sign in with a ChatGPT / Codex or Grok subscription
hya oauth login --provider codex --type openai-codex
hya oauth login --provider grok --type grok-build
```

---

## 3. Ask Hya to read and change code

Now type a real request in the message box and press **Enter**:

```text
Read this folder and tell me how the project is structured.
```

Want to point Hya at a specific file? Type `@` followed by part of the filename (for example `@README.md`). A file menu pops up—press **Tab** or **Enter** to insert it:

```text
Add a short troubleshooting section to @README.md
```

### Approving actions

When Hya wants to edit a file or run a terminal command, a yellow permission prompt appears above the input box:

- Press **`1`** to **Allow once**.
- Press **`2`** to **Always allow** this command in the future.
- Press **`3`** (or **Esc**) to **Deny** it.

---

## 4. Check changes or undo them

- Type `/diff` to see every line changed in your Git folder side-by-side. Press **Esc** to close it.
- Don't like what Hya just changed? Type `/undo`. Hya removes the last turn *and* puts your edited files back the way they were before. (`/redo` brings them back if you change your mind.)

---

## 5. Handy keys to remember

You don't need to memorize everything—press **`?`** on an empty input box anytime to search every key and command. Here are the ones you'll use every day:

| Input | What it does |
| --- | --- |
| `Enter` | Send your message |
| `Shift+Enter` or `Ctrl+J` | Add a new line without sending |
| `@filename` | Pick a file from your project to mention |
| `!git status` | Run a shell command directly inside the chat |
| `/` (on empty input) | Open the slash-command menu |
| `/model` | Switch AI models |
| `/effort` | Change how hard the model thinks (`low`, `high`, `max`) |
| `/new` | Start a fresh chat |
| `/sessions` | Switch between past chats (`Ctrl+A` shows archived ones) |
| `Esc` | Stop a running turn, or close an open menu |
| `Ctrl+C` twice (or `/exit`) | Close Hya and archive the current chat |
| `Ctrl+D` (on empty input) | Close Hya while leaving the chat running in the background |

Next up: see **[Common Workflows](common-workflows.md)** for practical ways to use Hya on real tasks.
