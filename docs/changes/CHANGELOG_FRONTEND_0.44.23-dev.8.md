# 0.44.23-dev.8

## Provider login

- Add `/key login <provider>` with nested completion for presets and configured IDs. ChatGPT uses browser authorization; new DeepSeek/OpenAI API connections open concealed key entry with defaults; custom names open connection setup. Existing routes retain their URL and protocol.
- Replace the dedicated ChatGPT key with the shared provider-view `k` login action.
