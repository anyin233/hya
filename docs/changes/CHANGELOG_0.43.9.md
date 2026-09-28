# 0.43.9

## A provider stream that drops mid-round is retried instead of ending the turn

- When a provider stream fails after it started (for example `Transport error: error decoding response body` after minutes of thinking) and the failed attempt produced no text and no tool call, the engine now retries that round up to 2 more times (backoff about 1 s, then 2 s; cancellation during the wait stops the turn at once). Previously the whole turn ended with an error and needed a manual "continue".
- Retried errors: transport failures, HTTP 429/5xx, and truncated-body decode errors. Rounds that already produced text or a tool call, cancellations, and store or tool errors are never retried.
- Each failed attempt is recorded as its own step (`StepStarted` then `StepFinished { finish: error }`); its partial reasoning stays in the transcript. Once the retries are used up, the turn ends with the original error as before.
- Engine-level round retries are separate from the provider's own zero-event request replay, which still applies before any event arrives.
