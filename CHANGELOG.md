# 0.48.0-dev.7

- `hya exec --bare --time-budget <SECS>` announces the run's wall-clock budget, sends staged deadline guidance, and caps Bash calls to the time available. The outside harness enforces the process deadline.
