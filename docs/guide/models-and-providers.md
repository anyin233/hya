# Models, Providers & Auth

Hya is not locked to any single AI company. You can connect Anthropic, OpenAI, Google Gemini, xAI Grok, OpenRouter, or a local server like Ollama, vLLM, or LM Studio—and switch between them mid-chat.

If you haven't set up any provider yet, Hya uses its built-in **`hya/offline`** model, which echoes your messages back locally so you can test the app offline.

---

## Option 1: Set up inside Hya with `/key` (Easiest)

Open `hya`, type **`/key`**, and press **Enter**.

- Press **`a`** to add a new provider (give it an ID like `anthropic`, pick its protocol, enter the base URL, and paste your API key).
- Press **`k`** on an existing provider to set or replace its API key.
- Press **`r`** to pull the latest list of models from that provider.
- Open a provider with **Enter** and press **`t`** on any model to send a tiny `"hi"` test ping—you'll see in under a second whether your key and URL work!

Changes you make in `/key` take effect **immediately**—no restart needed.

### Which protocol should I pick?

| Protocol (`kind`) | Use it for | Typical `base_url` |
| --- | --- | --- |
| `anthropic` | Anthropic Claude API (and Anthropic-compatible gateways) | `https://api.anthropic.com/v1` |
| `openai-response` | OpenAI's modern Responses API (`gpt-5`, reasoning models, native compaction) | `https://api.openai.com/v1` |
| `openai` | Standard OpenAI Chat Completions (OpenRouter, DeepSeek, Ollama, vLLM, LiteLLM) | `https://openrouter.ai/api/v1` or `http://127.0.0.1:11434/v1` |
| `google` | Google Gemini API | `https://generativelanguage.googleapis.com/v1beta` |

---

## Option 2: Sign in with a subscription (OAuth)

If you use a ChatGPT / Codex subscription or xAI Grok Build, you can sign in through your browser instead of managing raw API keys:

```sh
# ChatGPT / OpenAI Codex (prints a URL and one-time code—works great over SSH or WSL!)
hya oauth login --provider codex --type openai-codex

# xAI SuperGrok / Grok Build
hya oauth login --provider grok --type grok-build

# Check login status anytime
hya oauth status
```

Add `--browser` if you want Hya to open the login URL in your default browser automatically.

---

## Option 3: Configure `~/.config/hya/config.yaml`

Prefer keeping your setup in a config file you can back up in dotfiles? Edit `~/.config/hya/config.yaml`:

```yaml
default_model: anthropic/claude-sonnet-4-6

providers:
  anthropic:
    kind: anthropic
    base_url: https://api.anthropic.com/v1
    api_key: "{env:ANTHROPIC_API_KEY}"
    models:
      - claude-sonnet-4-6
      - claude-opus-4-6

  local:
    kind: openai
    base_url: http://127.0.0.1:11434/v1
    models:
      - id: qwen3-coder
        limit:
          context: 65536
          output: 8192
```

Notice `"{env:ANTHROPIC_API_KEY}"`—that tells Hya to read the key from your environment variable so you never commit secrets to Git. Even better, you can leave `api_key` out of `config.yaml` completely and save the key separately in `~/.local/share/hya/auth/<provider>.yaml` (`0600` permissions) by running:

```sh
hya login anthropic "$ANTHROPIC_API_KEY"
```

Saved keys from `hya login` or `/key` always take priority over `api_key` in `config.yaml`.

> **Note:** If Hya's background server is already running when you edit `config.yaml` by hand in a text editor, run `hya serve restart` (or open `/key` and press `r`) so the server picks up your changes.

---

## Where provider kinds come from

Every `kind` a provider uses is registered when Hya starts. The core always registers `openai` (aliases `openai-compatible` and `openai-completion`). The other kinds follow the built-in bundle selection (`bundles.builtin` in `config.yaml`) under these bundle ids:

| Bundle id | Kinds | OAuth type |
| --- | --- | --- |
| `hya/provider-anthropic` | `anthropic` | |
| `hya/provider-google` | `google` | |
| `hya/provider-openai` | `openai-response`, `openai-codex` | `openai-codex` |
| `hya/provider-grok` | `grok-build` | `grok-build` |

`profile: full` (the default) enables them all. `hya --bare` and `profile: bare` enable none, so only `openai` routes; list an id under `enable` to add its kinds back:

```yaml
bundles:
  builtin:
    profile: bare
    enable: [hya/provider-anthropic]
```

### Unsupported providers

A provider is `unsupported` when its `kind`, or the `kind` of one of its `models:` entries, is not registered, or when its saved OAuth credential has an OAuth type no registered kind provides. An unsupported provider gets no route and no models, but every other provider still works. Startup prints the reason on stderr, for example:

```text
hya: provider claude: kind `anthropic` is not provided by any enabled bundle (enabled kinds: openai, openai-compatible, openai-completion)
```

`hya models` does not list its models, and the `/v2` provider summary reports `result: unsupported`. Enable the bundle that provides the kind, or change the `kind`, and restart.

Adding or editing a provider (`/key`, `hya provider add`, `PUT /v2/providers/{id}`) checks only the kind it writes: an unregistered kind is rejected, and other providers with unregistered kinds do not block the edit.

---

## Switching models and thinking effort

Inside a chat:

- **`/model`** opens a searchable model picker showing every provider and context window size. Press **Enter** to switch.
- **`/effort`** (or `/think`) adjusts how hard reasoning models think before answering (`none`, `low`, `medium`, `high`, `xhigh`, `max`, or `default`).

Hya **remembers** both choices! When you pick a model for `hya-main`, future `hya-main` chats start on that model automatically, and each model remembers your preferred thinking effort across sessions.

Want to assign different models to different built-in agents (for instance, a big reasoning model for planning and a fast model for file scouting)? Open **`/agent`**, highlight `hya-scout`, and press **`m`** to set its model and **`t`** to set its thinking effort.

You can also pass a one-time thinking suffix anywhere a model name is accepted, like `openai/gpt-5#high`.

---

## Automatic failover with Categories

If a provider hits a rate limit (`429`) or goes down (`5xx`), **Categories** let Hya fall back to a backup model automatically when starting a turn or helper agent. Define them in `config.yaml`:

```yaml
categories:
  smart:
    - anthropic/claude-opus-4-6
    - openai/gpt-5
  fast:
    - anthropic/claude-haiku-4-5
    - google/gemini-2.5-flash
```

Agents and Bundles can ask for the `fast` category instead of hardcoding a single vendor, and Hya picks the first working candidate on your list.

For every provider and model field, see **[Configuration Reference](../reference/configuration.md)**.
