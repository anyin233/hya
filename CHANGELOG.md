# 0.48.0-dev.7

- `hya exec --bare --time-budget <SECS>` announces the run's wall-clock budget, sends staged deadline guidance, and caps Bash calls to the time available. The outside harness enforces the process deadline.
- The bare kernel's `read` tool reads PNG, JPEG, GIF, and WebP files and sends them to the model as image content.
- The bare kernel's system prompt asks the model to check the final deliverable against the task's most direct evidence, to resolve a contradicting tool result first, and to justify replacing a result that passed a check.
