# 0.48.0-dev.23

- **A tool call with malformed arguments no longer ends the run.** When a model sends argument text that is not valid JSON (cut off, or with trailing characters), the call is recorded and comes back to the model as a failed call that quotes the parser's error and asks for the call again; no tool, hook or permission check sees it. Before, the round failed with `decode: tool <name> arguments: …` and the run ended. Bare and full mode.
- **Bare `edit`: identical `old` and `new` is an error.** The call returns an input error and does not write the file; before, it returned `{"replaced": 1}` although nothing changed.
