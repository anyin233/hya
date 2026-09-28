# 0.37.12

## Restore pending tool approvals

The default interactions listing now includes pending permissions and questions.
OpenTUI users can see the command awaiting approval, respond with `/approve <id>`
or `/deny <id>`, and let the agent continue after the tool call. Explicit type
filters still select only the requested interaction kind.
