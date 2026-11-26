# How should a streaming client admit a turn that can ask for permission?

Subscribe to session events first, then call `V1Sdk::create_turn` and retain the returned turn ID. While the turn runs, keep receiving updates and dispatch permission replies. After `WaitTurn` reports completion, replay durable events from the mirror watermark before answering the editor.

`V1Sdk::prompt` waits for completion. A client that waits for that call before reading interactions can deadlock with a tool awaiting permission. When the client detaches, `cancel_matching_turn` protects a later turn started in the same session. The ACP permission-and-MCP process test exercises both sides of this lifecycle.
