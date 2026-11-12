# 0.45.11

## Headless execution and run deadlines

- Continue `hya exec` headless runs while the lead has live subagents, unfinished todos, or an announced next step instead of stopping after an intermediate progress turn.
- Add `--time-budget <seconds>` to `hya exec` with staged in-turn deadline notices and automatic timeout capping on `bash` and `wait` calls to the remaining run time.

## Default agents

- Update `hya-main` and `hya-reviewer` prompts around tool discipline, deliverable-first execution, fidelity verification, and leaving a clean workspace on exit.

## Providers

- Load OS native root certificates alongside bundled WebPKI roots for HTTPS model routes so TLS-terminating proxies trusted by the host or `SSL_CERT_FILE` work out of the box.
