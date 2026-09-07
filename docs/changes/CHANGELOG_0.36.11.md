# 0.36.11

## Fix Agent model execution

- Apply the backend-committed effective model to normal and targeted current-Agent selections before the next prompt.
- Keep failed mutations and selections for another Agent out of active request state while preserving explicit routing precedence.
- Add real hya-ts/backend fake-provider regressions for open-Session selection, per-Agent isolation, restart, and exact provider model identity.
