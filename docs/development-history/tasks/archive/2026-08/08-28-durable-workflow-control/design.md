# Design

The parent design sections **Durable Workflow State**, **App Control Module**, and **CLI, Tool, Server, and SDK** are authoritative.

## Durable Seam

Workflow lifecycle is appended to the owning root Session and folded only by `hya_proto::Projection`. Events carry identity, revision, Stage plan/status, and canonical Member references; they never carry directives, inputs, outputs, or child transcripts.

Projection rules are run-id fenced, declaration ordered, idempotent, and terminal-sticky. Activity is a join to canonical Members. Catalog availability is a current-runtime decoration over persisted identity, not a second durable model.

## Control Seam

`hya_app::WorkflowControl::execute` is the one caller interface for list/info/select/state/run. It owns binding, catalog/revision resolution, dead-owner reconciliation, idempotent admission, RunRegistry interaction, and dispatch to `hya_core::run_workflow`.

Tool calls retain immutable binding plus stable `ToolOperation`; direct calls mint or accept a `WorkflowRunId`. CLI/tool await completion. HTTP/slash return after durable admission and observe progress via Events.

## Surface Adapters

Backend CLI, hya-tool sink, native server, legacy Compat, Compat v2, and hya-sdk are adapters at this seam. Slash parsing occurs before model admission. Session DTOs and `session.updated` expose shared projected state. SDK uses the existing transport and does not add a reducer or client.
