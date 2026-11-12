# What closes a root turn stopped before its assistant message starts?

The turn that admitted the prompt. Ownership is explicit: the turn request
carries the admitted prompt's id, the turn lease records it, and only that turn
(or a drain/stop deadline closing it on its behalf) emits the empty assistant
start/finish pair for a message that never started. Earlier the engine inferred
ownership from the log ("the last message is an unanswered user prompt"); that
let a lease-less second caller answer a prompt another turn was running and
swallow background messages at the tail, so it was removed.

The server closes the window between admission and the owning task by
preparing first and admitting last: the HTTP handler prepares, reserves the
run, and spawns a task that commits the prompt and runs the turn. A dropped
handler can no longer strand a committed prompt, and a run reserved before a
stop admits nothing. `v1_restart` drives these through the real routes,
`AppState::stop_for_restart`, and `AppState::resume_interrupted_turns`.

A shell command stopped before `ToolCallRequested` still has no recorded
command text, so its restore prompt cannot name the command.
