# How should provider login scale without consuming more shortcut keys?

Use `/key login <provider>` with nested completion, plus the existing generic `k` login action in Provider View. The ChatGPT-specific `l` key was removed. `state/providerLogin.ts` owns preset metadata, completion names and flow selection. New preset providers can be added there without another command or key.

Configured declarations take precedence over presets: existing API routes get key replacement, existing openai-chatgpt routes get named account reauthorization, and their URL/protocol are retained. New DeepSeek and OpenAI API presets reuse the add-provider form at the concealed key step; an unknown name asks for protocol and URL. Existing Codex/Grok OAuth routes show their CLI login command instead of overwriting a refreshable credential with a key.

Opening login refreshes the catalog before selecting a flow, and a generation check prevents a late read reopening a closed or superseded form. Plain `/key` still manages providers. Syntax errors are explicit; keys/callbacks never belong in slash arguments.
