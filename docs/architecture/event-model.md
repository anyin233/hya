# Event Model

The event model lives in [`../../crates/yaca-proto`](../../crates/yaca-proto).
It is shared by the engine, store, provider layer, server, client, and TUI.

## Strong Ids

[`ids.rs`](../../crates/yaca-proto/src/ids.rs) defines distinct newtypes for:

- sessions
- messages
- parts
- tool calls
- team runs
- members
- goals
- loop runs
- permission requests

Each id wraps a UUIDv7 and displays with a prefix such as `ses_`, `msg_`, or
`tc_`. The strong types keep different ids from being accidentally swapped at
compile time.

## Events and Envelopes

[`event.rs`](../../crates/yaca-proto/src/event.rs) defines `Event`, the
canonical runtime stream. Major event groups are:

- session lifecycle
- session metadata, title, archive/share, agent/model switch, and status
- message lifecycle
- prompt context metadata
- message and part deletion
- step markers
- text streaming
- reasoning streaming
- text/reasoning replacement
- tool input and tool result lifecycle
- tool-part state updates
- runtime errors

An `Envelope` wraps an event with:

- `seq`: monotonic event sequence from the store.
- `ts_millis`: Unix epoch milliseconds.
- `event`: the event payload.

The envelope is the unit stored in SQLite replay results and streamed over SSE.

## Messages and Parts

[`message.rs`](../../crates/yaca-proto/src/message.rs) defines the model-facing
message shape:

| Type | Meaning |
| --- | --- |
| `Message::User` | User content as parts. |
| `Message::Assistant` | Assistant content, model/agent metadata, finish reason, optional usage. |
| `Message::System` | System content. |
| `Part::Text` | Text content. |
| `Part::Reasoning` | Reasoning text when a provider exposes it. |
| `Part::Media` | Canonical media attachment with MIME type, data, and optional filename. |
| `Part::Tool` | Tool call state, including input, output, and errors. |

Tool parts use `ToolPartState`:

```text
Pending -> Running -> Completed
                  \-> Error
```

## Projection

[`projection.rs`](../../crates/yaca-proto/src/projection.rs) folds ordered
envelopes into a `Projection`:

- `SessionCreated` sets session metadata.
- session metadata/title/archive/share/move/switch events update session state.
- `MessageStarted` creates a message row in memory.
- `UserPromptContextRecorded` preserves prompt attachment metadata.
- text and reasoning starts create parts.
- deltas append to existing parts.
- replacements overwrite existing text or reasoning part content.
- tool call requests upsert running tool parts.
- tool results, errors, and `ToolPartUpdated` finalize or replace tool state.
- delete events remove messages or parts from the projected view.
- `MessageFinished` records finish reason.

The reducer is idempotent by sequence number:

```rust
if env.seq.0 <= self.last_seq {
    return;
}
```

That means a caller can safely ignore duplicate older envelopes during replay or
after an SSE reconnect.

## Provider Boundary

Provider decoders produce canonical events, not provider-specific objects. For
example, OpenAI-compatible, Anthropic, and Google tool-call streams all become
`Event::ToolCallRequested`, even though their wire formats differ.

The engine is responsible for executing tool calls and appending
`Event::ToolResult` or `Event::ToolError`.

## Store Boundary

The store serializes `Event` JSON into `event_log.payload`. It does not maintain
a separate projection table for the current read path. `read_projection` replays
the session and folds through the shared reducer.
