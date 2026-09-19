# hya API v1 Reference

Generated from `proto/hya/v1` by `cargo xtask gen-api`; do not edit by hand.
The same contract is served over HTTP/JSON+SSE and gRPC. Every rpc lists its
HTTP binding (`method path`) and its fully-qualified gRPC method
(`hya.v1.<Service>.<Rpc>`).

Conventions: pagination uses opaque cursors; errors use the stable code
table (`hya_api::error`); timestamps are RFC 3339 strings in JSON.

## Contents

- [Auth service](#service-auth)
- [Catalog service](#service-catalog)
- [Events service](#service-events)
- [Files service](#service-files)
- [Interactions service](#service-interactions)
- [Logs service](#service-logs)
- [Mcp service](#service-mcp)
- [Messages service](#service-messages)
- [Process service](#service-process)
- [Project service](#service-project)
- [Pty service](#service-pty)
- [Session service](#service-session)
- [Turn service](#service-turn)
- [Workflow service](#service-workflow)
- [Worktrees service](#service-worktrees)
- [Messages](#messages)
- [Enums](#enums)

---

## Service `Auth`

Credential surface for model providers. This is hya's real auth system
(per-provider credentials in the user auth directory); it does not cover
third-party service connectors, which are out of scope for v1.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `SetProviderAuth` | `PUT /v1/auth/{provider_id}` | `hya.v1.Auth.SetProviderAuth` | `SetProviderAuthRequest` | `SetProviderAuthResponse` |
| `RemoveProviderAuth` | `DELETE /v1/auth/{provider_id}` | `hya.v1.Auth.RemoveProviderAuth` | `RemoveProviderAuthRequest` | `RemoveProviderAuthResponse` |
| `StartOauth` | `POST /v1/auth/{provider_id}/oauth/start` | `hya.v1.Auth.StartOauth` | `StartOauthRequest` | `StartOauthResponse` |
| `CompleteOauth` | `POST /v1/auth/{provider_id}/oauth/callback` | `hya.v1.Auth.CompleteOauth` | `CompleteOauthRequest` | `CompleteOauthResponse` |

### `Auth.SetProviderAuth`

Store an API key or refresh tokens for a provider.


### `Auth.RemoveProviderAuth`

Delete the stored credentials for a provider.


### `Auth.StartOauth`

Begin a provider OAuth flow; returns the authorization URL to open.


### `Auth.CompleteOauth`

Complete a provider OAuth flow with the callback code.


## Service `Catalog`

Read-only catalog surface used by pickers, completion UIs, and setup
flows.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListAgents` | `GET /v1/agents` | `hya.v1.Catalog.ListAgents` | `ListAgentsRequest` | `ListAgentsResponse` |
| `ListModels` | `GET /v1/models` | `hya.v1.Catalog.ListModels` | `ListModelsRequest` | `ListModelsResponse` |
| `ListProviders` | `GET /v1/providers` | `hya.v1.Catalog.ListProviders` | `ListProvidersRequest` | `ListProvidersResponse` |
| `GetProvider` | `GET /v1/providers/{provider_id}` | `hya.v1.Catalog.GetProvider` | `GetProviderRequest` | `ProviderInfo` |
| `ListCommands` | `GET /v1/commands` | `hya.v1.Catalog.ListCommands` | `ListCommandsRequest` | `ListCommandsResponse` |
| `ListSkills` | `GET /v1/skills` | `hya.v1.Catalog.ListSkills` | `ListSkillsRequest` | `ListSkillsResponse` |
| `ListTools` | `GET /v1/tools` | `hya.v1.Catalog.ListTools` | `ListToolsRequest` | `ListToolsResponse` |

### `Catalog.ListAgents`

Agents that can be bound to a session.


### `Catalog.ListModels`

Models across providers, optionally filtered to one provider.


### `Catalog.ListProviders`

Providers with their aggregate auth status.


### `Catalog.GetProvider`

One provider's detail including its models.


### `Catalog.ListCommands`

Slash-command catalog entries.


### `Catalog.ListSkills`

Skill catalog entries.


### `Catalog.ListTools`

Tool registry entries including hidden aliases.


## Service `Events`

Replay and live-stream surface.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListEvents` | `GET /v1/sessions/{session}/events` | `hya.v1.Events.ListEvents` | `ListEventsRequest` | `ListEventsResponse` |
| `StreamSessionEvents` | `GET /v1/sessions/{session}/events/stream (stream)` | `hya.v1.Events.StreamSessionEvents` | `StreamSessionEventsRequest` | `StreamFrame` |
| `StreamGlobalEvents` | `GET /v1/events/stream (stream)` | `hya.v1.Events.StreamGlobalEvents` | `StreamGlobalEventsRequest` | `StreamFrame` |

### `Events.ListEvents`

Replay events of one session after a sequence watermark.


### `Events.StreamSessionEvents`

Live stream for one session; server-streaming over gRPC and SSE over
HTTP. Emits `resync` when the consumer lags and must re-replay.


### `Events.StreamGlobalEvents`

Live stream across all sessions of a directory scope.


## Service `Files`

Filesystem reads for frontends (tree views, editors, go-to-symbol).

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ReadFile` | `GET /v1/fs/read` | `hya.v1.Files.ReadFile` | `ReadFileRequest` | `ReadFileResponse` |
| `ListDirectory` | `GET /v1/fs/list` | `hya.v1.Files.ListDirectory` | `ListDirectoryRequest` | `ListDirectoryResponse` |
| `FindFiles` | `GET /v1/fs/find` | `hya.v1.Files.FindFiles` | `FindFilesRequest` | `FindFilesResponse` |
| `SearchText` | `GET /v1/fs/search` | `hya.v1.Files.SearchText` | `SearchTextRequest` | `SearchTextResponse` |
| `SearchSymbols` | `GET /v1/fs/symbols` | `hya.v1.Files.SearchSymbols` | `SearchSymbolsRequest` | `SearchSymbolsResponse` |

### `Files.ReadFile`

Read one file's content.


### `Files.ListDirectory`

List one directory's entries.


### `Files.FindFiles`

Find files by glob-ish name pattern.


### `Files.SearchText`

Full-text search across files (ripgrep-backed).


### `Files.SearchSymbols`

Symbol search (definitions) across the directory.


## Service `Interactions`

Interaction surface shared by permission and question requests.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListInteractions` | `GET /v1/interactions` | `hya.v1.Interactions.ListInteractions` | `ListInteractionsRequest` | `ListInteractionsResponse` |
| `RespondInteraction` | `POST /v1/interactions/{request}/respond` | `hya.v1.Interactions.RespondInteraction` | `RespondInteractionRequest` | `RespondInteractionResponse` |
| `ListSavedRules` | `GET /v1/permissions/rules` | `hya.v1.Interactions.ListSavedRules` | `ListSavedRulesRequest` | `ListSavedRulesResponse` |
| `DeleteSavedRule` | `DELETE /v1/permissions/rules/{rule}` | `hya.v1.Interactions.DeleteSavedRule` | `DeleteSavedRuleRequest` | `DeleteSavedRuleResponse` |

### `Interactions.ListInteractions`

List pending permission/question requests, optionally scoped to one
session and filtered by type.


### `Interactions.RespondInteraction`

Respond to one pending request. Exactly one response kind is set.


### `Interactions.ListSavedRules`

List saved permission rules (persisted allow/deny/ask decisions).


### `Interactions.DeleteSavedRule`

Delete one saved permission rule.


## Service `Logs`

Frontends forward their own structured logs so backend logs correlate
with client-side behavior.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `IngestLog` | `POST /v1/logs` | `hya.v1.Logs.IngestLog` | `IngestLogRequest` | `IngestLogResponse` |

### `Logs.IngestLog`

Ingest one frontend log entry.


## Service `Mcp`

MCP control surface. Servers are managed as desired state mutated
through the app-owned MCP control handle; status composes desired and
observed state.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `GetMcpStatus` | `GET /v1/mcp` | `hya.v1.Mcp.GetMcpStatus` | `GetMcpStatusRequest` | `GetMcpStatusResponse` |
| `AddMcpServer` | `POST /v1/mcp` | `hya.v1.Mcp.AddMcpServer` | `AddMcpServerRequest` | `McpServerStatus` |
| `ConnectMcp` | `POST /v1/mcp/{name}/connect` | `hya.v1.Mcp.ConnectMcp` | `ConnectMcpRequest` | `McpServerStatus` |
| `DisconnectMcp` | `POST /v1/mcp/{name}/disconnect` | `hya.v1.Mcp.DisconnectMcp` | `DisconnectMcpRequest` | `McpServerStatus` |
| `StartMcpAuth` | `POST /v1/mcp/{name}/auth` | `hya.v1.Mcp.StartMcpAuth` | `StartMcpAuthRequest` | `StartMcpAuthResponse` |
| `CompleteMcpAuth` | `POST /v1/mcp/{name}/auth/complete` | `hya.v1.Mcp.CompleteMcpAuth` | `CompleteMcpAuthRequest` | `McpServerStatus` |
| `RemoveMcpAuth` | `DELETE /v1/mcp/{name}/auth` | `hya.v1.Mcp.RemoveMcpAuth` | `RemoveMcpAuthRequest` | `RemoveMcpAuthResponse` |

### `Mcp.GetMcpStatus`

Status of every configured MCP server in a directory.


### `Mcp.AddMcpServer`

Add (or replace) one MCP server in desired state.


### `Mcp.ConnectMcp`

Connect one MCP server now.


### `Mcp.DisconnectMcp`

Disconnect one MCP server.


### `Mcp.StartMcpAuth`

Begin an MCP server OAuth flow.


### `Mcp.CompleteMcpAuth`

Complete an MCP server OAuth flow.


### `Mcp.RemoveMcpAuth`

Remove stored MCP server credentials.


## Service `Messages`

Read surface over the projected transcript. This is a view over the
event log via the shared projection — the only durable read model.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListMessages` | `GET /v1/sessions/{session}/messages` | `hya.v1.Messages.ListMessages` | `ListMessagesRequest` | `ListMessagesResponse` |
| `GetMessage` | `GET /v1/sessions/{session}/messages/{message}` | `hya.v1.Messages.GetMessage` | `GetMessageRequest` | `MessageInfo` |
| `DeleteMessagePart` | `DELETE /v1/sessions/{session}/messages/{message}/parts/{part}` | `hya.v1.Messages.DeleteMessagePart` | `DeleteMessagePartRequest` | `DeleteMessagePartResponse` |
| `GetSessionTodo` | `GET /v1/sessions/{session}/todo` | `hya.v1.Messages.GetSessionTodo` | `GetSessionTodoRequest` | `TodoList` |

### `Messages.ListMessages`

List the transcript messages of a session.


### `Messages.GetMessage`

Fetch one message with its parts.


### `Messages.DeleteMessagePart`

Delete one part of a message (message editing / redaction).


### `Messages.GetSessionTodo`

Read the session's todo list projection.


## Service `Process`

Process-wide surface: health, location metadata, the runtime config bag,
process disposal/upgrade, and the one-round-trip bootstrap snapshot.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `GetHealth` | `GET /v1/health` | `hya.v1.Process.GetHealth` | `GetHealthRequest` | `GetHealthResponse` |
| `GetLocation` | `GET /v1/location` | `hya.v1.Process.GetLocation` | `GetLocationRequest` | `LocationInfo` |
| `GetConfig` | `GET /v1/config` | `hya.v1.Process.GetConfig` | `GetConfigRequest` | `GetConfigResponse` |
| `UpdateConfig` | `PATCH /v1/config` | `hya.v1.Process.UpdateConfig` | `UpdateConfigRequest` | `GetConfigResponse` |
| `DisposeProcess` | `POST /v1/process/dispose` | `hya.v1.Process.DisposeProcess` | `DisposeProcessRequest` | `DisposeProcessResponse` |
| `UpgradeProcess` | `POST /v1/process/upgrade` | `hya.v1.Process.UpgradeProcess` | `UpgradeProcessRequest` | `UpgradeProcessResponse` |
| `GetBootstrap` | `GET /v1/bootstrap` | `hya.v1.Process.GetBootstrap` | `GetBootstrapRequest` | `Bootstrap` |

### `Process.GetHealth`

Liveness and version probe.


### `Process.GetLocation`

Identity of this backend process and the directory it serves.


### `Process.GetConfig`

Read the effective runtime configuration bag for a directory.


### `Process.UpdateConfig`

Deep-merge a JSON object into the runtime configuration bag.


### `Process.DisposeProcess`

Ask the backend process to shut down gracefully.


### `Process.UpgradeProcess`

Ask the backend to self-update via the verified updater.


### `Process.GetBootstrap`

Aggregated startup snapshot: config, catalogs, pending interactions,
and session summaries in a single round trip. Frontends call this once
at launch instead of fanning out over every list rpc.


## Service `Project`

Project and VCS surface for directory-aware frontends.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListProjects` | `GET /v1/projects` | `hya.v1.Project.ListProjects` | `ListProjectsRequest` | `ListProjectsResponse` |
| `GetCurrentProject` | `GET /v1/projects/current` | `hya.v1.Project.GetCurrentProject` | `GetCurrentProjectRequest` | `ProjectInfo` |
| `UpdateProject` | `PATCH /v1/projects/{project}` | `hya.v1.Project.UpdateProject` | `UpdateProjectRequest` | `ProjectInfo` |
| `ListProjectDirectories` | `GET /v1/projects/{project}/directories` | `hya.v1.Project.ListProjectDirectories` | `ListProjectDirectoriesRequest` | `ListProjectDirectoriesResponse` |
| `InitProjectGit` | `POST /v1/projects/{project}/init-git` | `hya.v1.Project.InitProjectGit` | `InitProjectGitRequest` | `InitProjectGitResponse` |
| `GetVcsStatus` | `GET /v1/vcs` | `hya.v1.Project.GetVcsStatus` | `GetVcsStatusRequest` | `VcsStatus` |
| `GetVcsDiff` | `GET /v1/vcs/diff` | `hya.v1.Project.GetVcsDiff` | `GetVcsDiffRequest` | `GetVcsDiffResponse` |
| `ApplyPatch` | `POST /v1/vcs/apply` | `hya.v1.Project.ApplyPatch` | `ApplyPatchRequest` | `ApplyPatchResponse` |

### `Project.ListProjects`

List known projects.


### `Project.GetCurrentProject`

The project served by this backend process.


### `Project.UpdateProject`

Update project metadata (display name).


### `Project.ListProjectDirectories`

List the work directories registered under a project.


### `Project.InitProjectGit`

Initialize git in a project that has no repository yet.


### `Project.GetVcsStatus`

Repository status: branch, head, and changed files.


### `Project.GetVcsDiff`

Unified diff of working-tree changes.


### `Project.ApplyPatch`

Apply a unified diff patch to the working tree.


## Service `Pty`

PTY session surface for embedded terminals.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListShells` | `GET /v1/pty/shells` | `hya.v1.Pty.ListShells` | `ListShellsRequest` | `ListShellsResponse` |
| `CreatePty` | `POST /v1/pty` | `hya.v1.Pty.CreatePty` | `CreatePtyRequest` | `PtySession` |
| `GetPty` | `GET /v1/pty/{id}` | `hya.v1.Pty.GetPty` | `GetPtyRequest` | `PtySession` |
| `UpdatePty` | `PUT /v1/pty/{id}` | `hya.v1.Pty.UpdatePty` | `UpdatePtyRequest` | `PtySession` |
| `DeletePty` | `DELETE /v1/pty/{id}` | `hya.v1.Pty.DeletePty` | `DeletePtyRequest` | `DeletePtyResponse` |
| `CreateConnectToken` | `POST /v1/pty/{id}/connect-token` | `hya.v1.Pty.CreateConnectToken` | `CreateConnectTokenRequest` | `CreateConnectTokenResponse` |
| `StreamPty` | `GET /v1/pty/{id}/connect (stream)` | `hya.v1.Pty.StreamPty` | `stream PtyClientFrame` | `PtyServerFrame` |

### `Pty.ListShells`

List available shell binaries on the host.


### `Pty.CreatePty`

Create a PTY session.


### `Pty.GetPty`

Read one PTY session's state.


### `Pty.UpdatePty`

Resize or otherwise update a PTY session.


### `Pty.DeletePty`

Terminate a PTY session.


### `Pty.CreateConnectToken`

Mint a one-time token authorizing a terminal connection.


### `Pty.StreamPty`

Bidirectional terminal stream: client sends input/resize/ping,
server replies output/exit/pong.


## Service `Session`

Session lifecycle surface. Sessions are the durable event-sourced roots;
every turn, message, and projection read hangs off a session id.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `CreateSession` | `POST /v1/sessions` | `hya.v1.Session.CreateSession` | `CreateSessionRequest` | `CreateSessionResponse` |
| `GetSession` | `GET /v1/sessions/{session}` | `hya.v1.Session.GetSession` | `GetSessionRequest` | `SessionInfo` |
| `ListSessions` | `GET /v1/sessions` | `hya.v1.Session.ListSessions` | `ListSessionsRequest` | `ListSessionsResponse` |
| `UpdateSession` | `PATCH /v1/sessions/{session}` | `hya.v1.Session.UpdateSession` | `UpdateSessionRequest` | `SessionInfo` |
| `DeleteSession` | `DELETE /v1/sessions/{session}` | `hya.v1.Session.DeleteSession` | `DeleteSessionRequest` | `DeleteSessionResponse` |
| `ForkSession` | `POST /v1/sessions/{session}/fork` | `hya.v1.Session.ForkSession` | `ForkSessionRequest` | `ForkSessionResponse` |
| `CompactSession` | `POST /v1/sessions/{session}/compact` | `hya.v1.Session.CompactSession` | `CompactSessionRequest` | `CompactSessionResponse` |
| `SummarizeSession` | `POST /v1/sessions/{session}/summarize` | `hya.v1.Session.SummarizeSession` | `SummarizeSessionRequest` | `SummarizeSessionResponse` |
| `RevertSession` | `POST /v1/sessions/{session}/revert` | `hya.v1.Session.RevertSession` | `RevertSessionRequest` | `RevertSessionResponse` |

### `Session.CreateSession`

Create a session, optionally as a child of an existing session and
optionally running the directory init turn.


### `Session.GetSession`

Fetch one session's projection summary.


### `Session.ListSessions`

List sessions, optionally scoped under one parent (subagent tree).


### `Session.UpdateSession`

Update mutable session fields: title, agent, model, background flag.


### `Session.DeleteSession`

Delete a session and its event log.


### `Session.ForkSession`

Fork a session into a new session id, copying events up to a watermark.


### `Session.CompactSession`

Compact a session's context using the configured method ladder.


### `Session.SummarizeSession`

Produce a summary message for a session (titles, handoffs).


### `Session.RevertSession`

Revert a session to an earlier watermark, or undo the last revert.


## Service `Turn`

Turn admission and control surface.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `CreateTurn` | `POST /v1/sessions/{session}/turns` | `hya.v1.Turn.CreateTurn` | `CreateTurnRequest` | `CreateTurnResponse` |
| `GetTurn` | `GET /v1/sessions/{session}/turns/{turn}` | `hya.v1.Turn.GetTurn` | `GetTurnRequest` | `TurnInfo` |
| `WaitTurn` | `POST /v1/sessions/{session}/turns/{turn}/wait` | `hya.v1.Turn.WaitTurn` | `WaitTurnRequest` | `TurnInfo` |
| `CancelTurn` | `POST /v1/sessions/{session}/turns/{turn}/cancel` | `hya.v1.Turn.CancelTurn` | `CancelTurnRequest` | `TurnInfo` |

### `Turn.CreateTurn`

Admit one turn into a session: a user prompt, a slash command, or a
direct shell execution. Returns as soon as the turn is admitted.


### `Turn.GetTurn`

Read the current state of one turn.


### `Turn.WaitTurn`

Block until the turn reaches a terminal state or the timeout elapses.


### `Turn.CancelTurn`

Request cancellation of a running turn (cooperative abort).


## Service `Workflow`

Workflow catalog and per-session execution surface.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListWorkflows` | `GET /v1/workflows` | `hya.v1.Workflow.ListWorkflows` | `ListWorkflowsRequest` | `ListWorkflowsResponse` |
| `GetWorkflowState` | `GET /v1/sessions/{session}/workflow` | `hya.v1.Workflow.GetWorkflowState` | `GetWorkflowStateRequest` | `WorkflowState` |
| `SubmitWorkflowCommand` | `POST /v1/sessions/{session}/workflow` | `hya.v1.Workflow.SubmitWorkflowCommand` | `SubmitWorkflowCommandRequest` | `SubmitWorkflowCommandResponse` |

### `Workflow.ListWorkflows`

List discovered workflow sources for a directory.


### `Workflow.GetWorkflowState`

Read the projected workflow state of one session.


### `Workflow.SubmitWorkflowCommand`

Submit one typed workflow command against a session (select source,
run, inspect). Mirrors the slash-command `/workflow` surface.


## Service `Worktrees`

Worktree create/list/delete surface backed by the engine's worktree
helpers.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListWorktrees` | `GET /v1/worktrees` | `hya.v1.Worktrees.ListWorktrees` | `ListWorktreesRequest` | `ListWorktreesResponse` |
| `CreateWorktree` | `POST /v1/worktrees` | `hya.v1.Worktrees.CreateWorktree` | `CreateWorktreeRequest` | `Worktree` |
| `DeleteWorktree` | `DELETE /v1/worktrees/{worktree}` | `hya.v1.Worktrees.DeleteWorktree` | `DeleteWorktreeRequest` | `DeleteWorktreeResponse` |
| `ResetWorktree` | `POST /v1/worktrees/{worktree}/reset` | `hya.v1.Worktrees.ResetWorktree` | `ResetWorktreeRequest` | `Worktree` |

### `Worktrees.ListWorktrees`

List worktrees of a repository.


### `Worktrees.CreateWorktree`

Create a new worktree (and its branch when needed).


### `Worktrees.DeleteWorktree`

Delete a worktree and optionally its branch.


### `Worktrees.ResetWorktree`

Reset a worktree to a clean state at its branch head.


## Messages

### `OauthTokens`

OAuth tokens captured from a completed provider flow.

| Field | Type | Description |
|---|---|---|
| `access_token` (1) | `string` | Access token issued by the provider. |
| `refresh_token` (2) | `string` | Refresh token when the provider issued one. |
| `expires_at` (3) | `int64` | Token expiry as unix epoch seconds; 0 when unknown. |

### `SetProviderAuthRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory context for auth resolution. |
| `provider_id` (2) | `string` | Provider identifier the credentials belong to. |
| `api_key` (3) | `oneof `secret`: string` | Credential payload: an API key or captured OAuth tokens. Raw API key string. |
| `oauth` (4) | `oneof `secret`: OauthTokens` | OAuth token pair captured by the client. |

### `SetProviderAuthResponse`


| Field | Type | Description |
|---|---|---|
| `status` (1) | `AuthStatus` | Resulting auth status for the provider. |

### `RemoveProviderAuthRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory context for auth resolution. |
| `provider_id` (2) | `string` | Provider identifier whose credentials should be deleted. |

### `StartOauthRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory context for auth resolution. |
| `provider_id` (2) | `string` | Provider identifier to authenticate. |

### `StartOauthResponse`


| Field | Type | Description |
|---|---|---|
| `authorization_url` (1) | `string` | Authorization URL the client must open in a browser. |
| `state` (2) | `string` | State value to echo back in `CompleteOauth`. |

### `CompleteOauthRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory context for auth resolution. |
| `provider_id` (2) | `string` | Provider identifier completing the flow. |
| `code` (3) | `string` | Authorization code returned by the provider callback. |
| `state` (4) | `string` | State value from `StartOauth` when the provider requires it. |

### `CompleteOauthResponse`


| Field | Type | Description |
|---|---|---|
| `status` (1) | `AuthStatus` | Resulting auth status for the provider. |

### `ModelRef`

Provider/model identity pair. `model_id` is provider-local.

| Field | Type | Description |
|---|---|---|
| `provider_id` (1) | `string` | Provider identifier as configured (e.g. `anthropic`). |
| `model_id` (2) | `string` | Provider-local model identifier (e.g. `claude-sonnet-4-6`). |
| `variant` (3) | `string` | Optional reasoning variant suffix (e.g. `high`). |

### `ListAgentsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory whose bound agent catalog should be listed. |
| `page` (2) | `PageRequest` | Standard pagination controls. |

### `AgentSummary`

One selectable agent.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Agent name used in `CreateSessionRequest.agent`. |
| `model` (2) | `ModelRef` | Default model the agent runs on. |
| `description` (3) | `string` | One-line description for pickers. |
| `hidden` (4) | `bool` | Whether the agent is hidden from default pickers. |

### `ListAgentsResponse`


| Field | Type | Description |
|---|---|---|
| `agents` (1) | `repeated AgentSummary` | Agents bound to the directory. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `ListModelsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory whose model catalog should be listed. |
| `provider_id` (2) | `string` | Restrict to one provider when non-empty. |
| `page` (3) | `PageRequest` | Standard pagination controls. |

### `ModelSummary`

One selectable model.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Fully qualified model reference string (`provider/model`). |
| `provider_id` (2) | `string` | Provider identifier. |
| `model_id` (3) | `string` | Provider-local model identifier. |
| `display_name` (4) | `string` | Display name when the provider publishes one. |
| `reasoning` (5) | `bool` | Whether the route supports reasoning effort variants. |
| `auth` (6) | `AuthStatus` | Auth state of the owning provider route. |

### `ListModelsResponse`


| Field | Type | Description |
|---|---|---|
| `models` (1) | `repeated ModelSummary` | Models matching the filter. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `ListProvidersRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory whose provider catalog should be listed. |
| `page` (2) | `PageRequest` | Standard pagination controls. |

### `ProviderSummary`

One provider route with aggregate auth state.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Provider identifier as configured. |
| `name` (2) | `string` | Human-readable provider name. |
| `auth` (3) | `AuthStatus` | Aggregate auth status across the provider's routes. |
| `website` (4) | `string` | Vendor documentation/auth URL when known. |
| `result` (5) | `string` | Model discovery outcome: `models`, `empty`, `unavailable`, `invalid`. |

### `ListProvidersResponse`


| Field | Type | Description |
|---|---|---|
| `providers` (1) | `repeated ProviderSummary` | Providers visible in this directory. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `GetProviderRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory context for the provider lookup. |
| `provider_id` (2) | `string` | Provider identifier. |

### `ProviderInfo`

Provider detail with its model rows.

| Field | Type | Description |
|---|---|---|
| `summary` (1) | `ProviderSummary` | Provider summary. |
| `models` (2) | `repeated ModelSummary` | Models exposed by this provider. |
| `supports_api_key` (3) | `bool` | Whether an API-key auth method is supported. |
| `supports_oauth` (4) | `bool` | Whether an OAuth flow is supported. |

### `ListCommandsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory whose command catalog should be listed. |
| `page` (2) | `PageRequest` | Standard pagination controls. |

### `CommandSummary`

One slash-command entry.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Command name without the leading `/`. |
| `description` (2) | `string` | One-line description for completion UIs. |
| `argument_hint` (3) | `string` | Argument hint shown after the command name. |
| `hints` (5) | `repeated string` | Positional/flag hints from the command template, in template order. |
| `source` (6) | `string` | Where the command was discovered (`command`, `skill`, ...). |
| `template` (7) | `string` | Expansion template (positional `$1`/`$ARGUMENTS` placeholders). |
| `agent` (8) | `string` | Agent the command run binds when authored. |
| `model` (9) | `string` | Model the command run binds when authored. |
| `subtask` (10) | `optional bool` | Whether the command runs as a detached subtask. |

### `ListCommandsResponse`


| Field | Type | Description |
|---|---|---|
| `commands` (1) | `repeated CommandSummary` | Commands visible in this directory. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `ListSkillsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory whose skill catalog should be listed. |
| `page` (2) | `PageRequest` | Standard pagination controls. |

### `SkillSummary`

One invocable skill.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Skill name used with `/skill` and the skill plane. |
| `description` (2) | `string` | One-line description of what the skill does. |
| `source` (3) | `string` | Where the skill was discovered (`builtin`, `bundle`, `project`, ...). |
| `content` (4) | `string` | Full skill markdown body (frontmatter + content). |
| `location` (5) | `string` | Where the skill file lives (`<built-in>` for compiled-in skills). |

### `ListSkillsResponse`


| Field | Type | Description |
|---|---|---|
| `skills` (1) | `repeated SkillSummary` | Skills visible in this directory. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `ListToolsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory whose tool catalog should be listed. |
| `page` (2) | `PageRequest` | Standard pagination controls. |

### `ToolSummary`

One tool registry entry.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Canonical tool name. |
| `description` (2) | `string` | One-line description of the tool's contract. |
| `kind` (3) | `string` | Tool origin: `builtin`, `mcp`, or `plugin`. |
| `hidden` (4) | `bool` | Whether the name is a hidden alias. |

### `ListToolsResponse`


| Field | Type | Description |
|---|---|---|
| `tools` (1) | `repeated ToolSummary` | Tools visible in this directory. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `Error`

Stable, machine-readable error returned by every failed v1 call.

HTTP bindings render `{"error": {"code": ..., "message": ...}}` with the
status mapped from the code; gRPC bindings carry the same code in the
trailer/status details.

| Field | Type | Description |
|---|---|---|
| `code` (1) | `string` | Stable error code, e.g. `session_not_found`, `session_busy`. |
| `message` (2) | `string` | Human-readable explanation safe to show to an end user. |

### `PageRequest`

Standard list controls shared by every paginated rpc.

| Field | Type | Description |
|---|---|---|
| `cursor` (1) | `string` | Opaque cursor from a previous `PageInfo.next_cursor`; empty starts at the beginning. |
| `limit` (2) | `uint32` | Maximum entries to return; servers clamp to their own maximum. |

### `PageInfo`

Pagination outcome attached to every paginated response.

| Field | Type | Description |
|---|---|---|
| `next_cursor` (1) | `string` | Cursor to pass into the next `PageRequest`; empty when exhausted. |
| `has_more` (2) | `bool` | Whether more entries exist beyond this page. |

### `ListEventsRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier to replay. |
| `since_seq` (2) | `uint64` | Return only events with `seq` strictly greater than this value. |
| `limit` (3) | `uint32` | Maximum events to return; 0 uses the server default. |
| `include_raw` (4) | `bool` | When true, also return the canonical durable envelope JSON lines in `raw_envelopes` for tooling and test harnesses. The internal envelope shape is not a stable contract; clients must treat it as opaque. |

### `ListEventsResponse`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |
| `events` (2) | `repeated StreamEvent` | Replayed events in sequence order. |
| `next_seq` (3) | `uint64` | Highest `seq` contained in this response; pass as the next `since_seq`. |
| `raw_envelopes` (4) | `repeated string` | Canonical durable envelope JSON lines, present only when the request set `include_raw`. Internal shape; treat as opaque beyond replay. |

### `StreamSessionEventsRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier to stream. |
| `since_seq` (2) | `uint64` | Emit events after this watermark first, then continue live. |

### `StreamGlobalEventsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `since_seq` (2) | `uint64` | Emit events after this watermark first, then continue live. |

### `StreamFrame`

One frame on a live stream.

| Field | Type | Description |
|---|---|---|
| `event` (1) | `oneof `frame`: StreamEvent` | Frame payload; exactly one kind is set. A projected event. |
| `resync` (2) | `oneof `frame`: ResyncFrame` | Lag signal: replay from `last_seq` to recover. |

### `ResyncFrame`

Typed lag signal replacing the legacy SSE `resync` event name.

| Field | Type | Description |
|---|---|---|
| `last_seq` (1) | `uint64` | Highest sequence number the server guarantees delivered; resume with `since_seq = last_seq`. |

### `StreamEvent`

One curated projected event from the event log.

| Field | Type | Description |
|---|---|---|
| `seq` (1) | `uint64` | Monotonic sequence number within the session. |
| `session` (2) | `string` | Owning session identifier. |
| `time_recorded` (3) | `google.protobuf.Timestamp` | When the event was recorded. |
| `session_started` (4) | `oneof `payload`: SessionStarted` | Event payload; exactly one kind is set. A session was created. |
| `session_updated` (5) | `oneof `payload`: SessionUpdated` | Session metadata changed (title, model, agent, background). |
| `message_started` (6) | `oneof `payload`: MessageStarted` | A message started (user admitted or assistant round began). |
| `message_finished` (7) | `oneof `payload`: MessageFinished` | A message reached a terminal state. |
| `part_started` (8) | `oneof `payload`: PartStarted` | A part was appended to a message. |
| `part_appended` (9) | `oneof `payload`: PartAppended` | Content was appended to a streaming part (text/reasoning deltas). |
| `part_completed` (10) | `oneof `payload`: PartCompleted` | A part reached its final form. |
| `tool_state_changed` (11) | `oneof `payload`: ToolStateChanged` | A tool call's execution state changed. |
| `permission_requested` (12) | `oneof `payload`: PermissionRequested` | A permission decision is pending. |
| `question_requested` (13) | `oneof `payload`: QuestionRequested` | A question is pending. |
| `interaction_resolved` (14) | `oneof `payload`: InteractionResolved` | A pending interaction was resolved. |
| `todo_updated` (15) | `oneof `payload`: TodoUpdated` | The session todo list changed. |
| `workflow_updated` (16) | `oneof `payload`: WorkflowUpdated` | The workflow projection changed. |
| `tokens_recorded` (17) | `oneof `payload`: TokensRecorded` | Token usage was recorded for a round. |
| `compaction_applied` (18) | `oneof `payload`: CompactionApplied` | A compaction strategy was applied to the context. |
| `session_deleted` (19) | `oneof `payload`: SessionDeleted` | A session was deleted. |

### `SessionStarted`

A session was created.

| Field | Type | Description |
|---|---|---|
| `agent` (1) | `string` | Agent name bound to the new session. |
| `model` (2) | `string` | Model reference string the session starts on. |
| `workdir` (3) | `string` | Absolute working directory of the session. |
| `parent` (4) | `string` | Parent session id when this is a child. |

### `SessionUpdated`

Session metadata changed.

| Field | Type | Description |
|---|---|---|
| `title` (1) | `optional string` | New title when changed. |
| `model` (2) | `optional string` | New model reference string when changed. |
| `agent` (3) | `optional string` | New agent name when changed. |
| `background` (4) | `optional bool` | New background flag when changed. |

### `MessageStarted`

A session was deleted.
A message started.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Message identifier. |
| `role` (2) | `Role` | Author role. |
| `agent` (3) | `string` | Agent name attributed when applicable. |
| `model` (4) | `string` | Model producing the message when applicable. |

### `MessageFinished`

A message reached a terminal state.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Message identifier. |
| `finish` (2) | `FinishReason` | Terminal finish reason. |
| `usage` (3) | `TokenUsage` | Token usage of the final round when the backend accounts it here. |

### `PartStarted`

A part was appended to a message.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Owning message identifier. |
| `part` (2) | `string` | Part identifier. |
| `kind` (3) | `string` | Part kind discriminator matching `PartInfo.kind`. |

### `PartAppended`

Streaming delta appended to a part.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Owning message identifier. |
| `part` (2) | `string` | Part identifier. |
| `text_delta` (3) | `string` | Incremental text delta for text/reasoning parts. |

### `PartCompleted`

A part reached its final form.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Owning message identifier. |
| `part` (2) | `string` | Part identifier. |

### `ToolStateChanged`

A tool call's execution state changed.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Owning message identifier. |
| `part` (2) | `string` | Part identifier of the tool call. |
| `call_id` (3) | `string` | Matching call id. |
| `state` (4) | `ToolExecutionState` | New execution state. |
| `error_code` (5) | `string` | Stable error code when the call failed. |

### `PermissionRequested`

A permission decision is pending.

| Field | Type | Description |
|---|---|---|
| `request` (1) | `string` | Pending interaction id; respond via `Interactions.RespondInteraction`. |
| `interaction` (2) | `Interaction` | Interaction summary (title, options, payload). |

### `QuestionRequested`

A question is pending.

| Field | Type | Description |
|---|---|---|
| `request` (1) | `string` | Pending interaction id; respond via `Interactions.RespondInteraction`. |
| `interaction` (2) | `Interaction` | Interaction summary (title, options, payload). |

### `InteractionResolved`

A pending interaction was resolved.

| Field | Type | Description |
|---|---|---|
| `request` (1) | `string` | Resolved interaction id. |

### `TodoUpdated`

The session todo list changed.

| Field | Type | Description |
|---|---|---|
| `items` (1) | `repeated TodoItem` | Full replacement todo list. |

### `WorkflowUpdated`

The workflow projection changed.

| Field | Type | Description |
|---|---|---|
| `state` (1) | `WorkflowState` | Full replacement workflow state. |

### `TokensRecorded`

Token usage was recorded for a round.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Message the round belongs to. |
| `usage` (2) | `TokenUsage` | Usage recorded for the round. |

### `CompactionApplied`

A compaction strategy was applied to the context.

| Field | Type | Description |
|---|---|---|
| `until_seq` (1) | `uint64` | Watermark the context was compacted up to. |
| `strategy` (2) | `string` | Strategy that fired (`shake`, `remote`, `soft`, `snap_compact`, `handoff`). |

### `ReadFileRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `path` (2) | `string` | File path relative to the directory. |
| `max_bytes` (3) | `uint64` | Truncate after this many bytes; 0 reads the whole file. |

### `ReadFileResponse`


| Field | Type | Description |
|---|---|---|
| `content` (1) | `bytes` | File content; raw bytes for binary files. |
| `text` (2) | `bool` | Whether `content` decodes as UTF-8 text. |
| `mime` (3) | `string` | Guessed MIME type. |

### `ListDirectoryRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `path` (2) | `string` | Subdirectory path relative to the directory; empty lists the root. |

### `DirEntry`

One directory entry.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Entry name within its parent directory. |
| `kind` (2) | `DirEntryKind` | Entry kind. |
| `size` (3) | `uint64` | File size in bytes when known. |
| `time_modified` (4) | `google.protobuf.Timestamp` | Modification time when known. |

### `ListDirectoryResponse`


| Field | Type | Description |
|---|---|---|
| `entries` (1) | `repeated DirEntry` | Entries in stable name order. |

### `FindFilesRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `pattern` (2) | `string` | Glob pattern matched against relative paths (`**/*.rs`). |
| `limit` (3) | `uint32` | Maximum paths to return; 0 uses the server default. |

### `FindFilesResponse`


| Field | Type | Description |
|---|---|---|
| `paths` (1) | `repeated string` | Matching relative paths. |

### `SearchTextRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `query` (2) | `string` | Search query (regex when the backend enables it, else literal). |
| `glob` (3) | `string` | Restrict search to files matching this glob; empty searches all. |
| `limit` (4) | `uint32` | Maximum matches to return; 0 uses the server default. |

### `TextMatch`

One text search match.

| Field | Type | Description |
|---|---|---|
| `path` (1) | `string` | Relative file path. |
| `line_number` (2) | `uint32` | 1-based line number. |
| `text` (3) | `string` | Matched line content. |

### `SearchTextResponse`


| Field | Type | Description |
|---|---|---|
| `matches` (1) | `repeated TextMatch` | Matches in path order. |

### `SearchSymbolsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `query` (2) | `string` | Symbol name query. |
| `limit` (3) | `uint32` | Maximum symbols to return; 0 uses the server default. |

### `Symbol`

One discovered symbol.

| Field | Type | Description |
|---|---|---|
| `path` (1) | `string` | Relative file path. |
| `name` (2) | `string` | Symbol name. |
| `kind` (3) | `SymbolKind` | Symbol kind. |
| `line_number` (4) | `uint32` | 1-based line number. |

### `SearchSymbolsResponse`


| Field | Type | Description |
|---|---|---|
| `symbols` (1) | `repeated Symbol` | Symbols matching the query. |

### `Interaction`

One pending interaction request.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Interaction identifier used in `RespondInteraction`. |
| `session` (2) | `string` | Owning session identifier; empty for process-wide requests. |
| `type` (3) | `InteractionType` | Whether this is a permission or a question request. |
| `title` (4) | `string` | Short human-readable title (for example the tool call summary). |
| `detail` (5) | `string` | Longer explanation body when the backend provides one. |
| `options` (6) | `repeated string` | Selectable option labels when the request is multiple-choice. |
| `payload` (7) | `google.protobuf.Struct` | Structured payload (tool input, question metadata) as a JSON object. |
| `time_created` (8) | `google.protobuf.Timestamp` | When the request was created. |

### `ListInteractionsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `session` (2) | `string` | Restrict to one session when non-empty. |
| `type` (3) | `InteractionType` | Restrict to one interaction type when set. |
| `page` (4) | `PageRequest` | Standard pagination controls. |

### `ListInteractionsResponse`


| Field | Type | Description |
|---|---|---|
| `interactions` (1) | `repeated Interaction` | Pending requests, oldest first. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `PermissionResponse`

Response to a permission request.

| Field | Type | Description |
|---|---|---|
| `allowed` (1) | `bool` | Whether the tool call is allowed to proceed. |
| `persist` (2) | `bool` | Persist the decision as a saved rule for future matching calls. |

### `QuestionResponse`

Response to a question request.

| Field | Type | Description |
|---|---|---|
| `answer` (1) | `string` | Chosen answer text (a free-form answer or one of `options`). |
| `rejected` (2) | `bool` | Reject the question instead of answering it. |

### `RespondInteractionRequest`


| Field | Type | Description |
|---|---|---|
| `request` (1) | `string` | Interaction identifier being responded to. |
| `permission` (2) | `oneof `response`: PermissionResponse` | Response payload; exactly one kind is set and must match the request type. Answer a permission request. |
| `question` (3) | `oneof `response`: QuestionResponse` | Answer a question request. |

### `RespondInteractionResponse`


| Field | Type | Description |
|---|---|---|
| `applied` (1) | `bool` | Whether the response was applied to a still-pending request. False means the request was already resolved elsewhere (idempotent replay). |

### `SavedRule`

A persisted permission decision.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Rule identifier. |
| `permission` (2) | `RulePermission` | Effect of the rule. |
| `tool` (3) | `string` | Tool name the rule matches; empty matches every tool. |
| `pattern` (4) | `string` | Pattern the rule matches (command prefix, path prefix, ...). |
| `time_created` (5) | `google.protobuf.Timestamp` | When the rule was saved. |

### `ListSavedRulesRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `page` (2) | `PageRequest` | Standard pagination controls. |

### `ListSavedRulesResponse`


| Field | Type | Description |
|---|---|---|
| `rules` (1) | `repeated SavedRule` | Saved rules in stable id order. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `DeleteSavedRuleRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `rule` (2) | `string` | Rule identifier to delete. |

### `IngestLogRequest`


| Field | Type | Description |
|---|---|---|
| `service` (1) | `string` | Which frontend surface emitted the entry (for example `tui`). |
| `level` (2) | `LogLevel` | Entry severity. |
| `message` (3) | `string` | Entry message text. |
| `extra` (4) | `google.protobuf.Struct` | Structured extras as a JSON object. |

### `McpServerStatus`

Status of one MCP server.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Server name as configured. |
| `state` (2) | `McpServerState` | Connection state. |
| `tools` (3) | `repeated string` | Namespaced tools exposed by this server (`mcp__server__tool`). |
| `error` (4) | `string` | Human-readable error when state is FAILED. |
| `auth_required` (5) | `bool` | Whether the server requires an OAuth login. |

### `GetMcpStatusRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |

### `GetMcpStatusResponse`


| Field | Type | Description |
|---|---|---|
| `servers` (1) | `repeated McpServerStatus` | Status of every configured server. |

### `CommandTransport`

stdio transport: launch a local command.

| Field | Type | Description |
|---|---|---|
| `command` (1) | `string` | Executable to launch. |
| `args` (2) | `repeated string` | Arguments passed to the executable. |
| `string> env` (3) | `map<string,` | Extra environment variables for the child process. |

### `UrlTransport`

HTTP/SSE transport: connect to a URL.

| Field | Type | Description |
|---|---|---|
| `url` (1) | `string` | Server base URL. |

### `AddMcpServerRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `name` (2) | `string` | Server name used in tool namespaces. |
| `command` (3) | `oneof `transport`: CommandTransport` | Transport definition; exactly one kind is set. Launch a local stdio server. |
| `url` (4) | `oneof `transport`: UrlTransport` | Connect to a remote HTTP server. |
| `enabled` (5) | `optional bool` | Whether the server starts enabled; defaults to true. |

### `ConnectMcpRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `name` (2) | `string` | Server name to connect. |

### `DisconnectMcpRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `name` (2) | `string` | Server name to disconnect. |

### `StartMcpAuthRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `name` (2) | `string` | Server name to authenticate. |

### `StartMcpAuthResponse`


| Field | Type | Description |
|---|---|---|
| `authorization_url` (1) | `string` | Authorization URL the client must open in a browser. |

### `CompleteMcpAuthRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `name` (2) | `string` | Server name completing the flow. |
| `code` (3) | `string` | Authorization code returned by the provider callback. |

### `RemoveMcpAuthRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `name` (2) | `string` | Server name whose credentials should be removed. |

### `TokenUsage`

Token accounting for one model round.

| Field | Type | Description |
|---|---|---|
| `input` (1) | `uint64` | Billed input tokens of the round. |
| `output` (2) | `uint64` | Billed output tokens of the round. |
| `reasoning` (3) | `uint64` | Reasoning tokens counted within the round. |
| `cache_read` (4) | `uint64` | Input tokens served from cache. |
| `cache_write` (5) | `uint64` | Input tokens written to cache. |

### `TextPart`

Text part of a message.

| Field | Type | Description |
|---|---|---|
| `text` (1) | `string` | Concatenated text content so far. |

### `ReasoningPart`

Model reasoning trace part.

| Field | Type | Description |
|---|---|---|
| `text` (1) | `string` | Concatenated reasoning content so far. |
| `variant` (2) | `string` | Reasoning variant tag when the route exposes one. |

### `ToolCallPart`

A tool invocation requested by the model.

| Field | Type | Description |
|---|---|---|
| `call_id` (1) | `string` | Engine-issued call id used to match the result part. |
| `tool` (2) | `string` | Canonical tool name. |
| `input_json` (3) | `string` | Tool input as a JSON object. |
| `state` (4) | `ToolExecutionState` | Execution state of the call. |
| `error_code` (5) | `string` | Stable structured error type when the call failed (e.g. `unknown`). |
| `error_message` (6) | `string` | Structured error message when the call failed. |

### `ToolResultPart`

The outcome of a tool invocation.

| Field | Type | Description |
|---|---|---|
| `call_id` (1) | `string` | Matching call id from `ToolCallPart.call_id`. |
| `output` (2) | `string` | Tool output payload (text or serialized JSON). |
| `error_code` (3) | `string` | Stable error code when the call failed. |
| `error_message` (4) | `string` | Human-readable error text when the call failed. |

### `AttachmentPart`

A binary or file attachment on a message.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Attachment file name. |
| `mime` (2) | `string` | MIME type when known. |
| `data` (3) | `bytes` | Inline payload when the backend stored it inline; empty otherwise. |
| `path` (4) | `string` | Path reference when the attachment is stored on disk. |

### `PartInfo`

One part of a message body.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Part identifier unique within the message. |
| `text` (2) | `oneof `kind`: TextPart` | Part payload; exactly one kind is set. Visible text content. |
| `reasoning` (3) | `oneof `kind`: ReasoningPart` | Reasoning trace content. |
| `tool_call` (4) | `oneof `kind`: ToolCallPart` | A tool invocation. |
| `tool_result` (5) | `oneof `kind`: ToolResultPart` | A tool invocation outcome. |
| `attachment` (6) | `oneof `kind`: AttachmentPart` | An attachment. |

### `MessageInfo`

Projection snapshot of one message.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Message identifier. |
| `session` (2) | `string` | Owning session identifier. |
| `role` (3) | `Role` | Author role. |
| `agent` (4) | `string` | Agent name attributed to the message when applicable. |
| `model` (5) | `string` | Model that produced the message when applicable. |
| `finish` (6) | `FinishReason` | Terminal finish reason for assistant messages. |
| `parts` (7) | `repeated PartInfo` | Ordered message parts. |
| `time_created` (8) | `google.protobuf.Timestamp` | When the message was created. |
| `time_updated` (9) | `google.protobuf.Timestamp` | When the message projection last changed. |

### `ListMessagesRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |
| `page` (2) | `PageRequest` | Standard pagination controls. |

### `ListMessagesResponse`


| Field | Type | Description |
|---|---|---|
| `messages` (1) | `repeated MessageInfo` | Messages in append order. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `GetMessageRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |
| `message` (2) | `string` | Message identifier. |

### `DeleteMessagePartRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |
| `message` (2) | `string` | Message identifier. |
| `part` (3) | `string` | Part identifier within the message. |

### `TodoItem`

One todo item of a session's plan.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Stable item identifier. |
| `content` (2) | `string` | Item content text. |
| `status` (3) | `TodoStatus` | Lifecycle status of the item. |

### `GetSessionTodoRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |

### `TodoList`

A session's todo list projection.

| Field | Type | Description |
|---|---|---|
| `items` (1) | `repeated TodoItem` | Ordered todo items. |

### `GetHealthResponse`


| Field | Type | Description |
|---|---|---|
| `ok` (1) | `bool` | Always `true` when the endpoint answers successfully. |
| `version` (2) | `string` | Backend version string (workspace release version). |

### `LocationInfo`

Where this backend runs and which directory it serves.

| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Absolute working directory this backend instance serves. |
| `hostname` (2) | `string` | Hostname of the machine running the backend. |
| `pid` (3) | `uint32` | OS process id of the backend. |
| `version` (4) | `string` | Backend version string (workspace release version). |

### `GetConfigRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory whose effective config should be read; empty means the process default directory. |

### `GetConfigResponse`


| Field | Type | Description |
|---|---|---|
| `values` (1) | `google.protobuf.Struct` | Effective merged configuration as a JSON object. |

### `UpdateConfigRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory whose config should be patched; empty means the process default directory. |
| `patch` (2) | `google.protobuf.Struct` | JSON object deep-merged into the stored config for that directory. |

### `UpgradeProcessResponse`


| Field | Type | Description |
|---|---|---|
| `status` (1) | `string` | Human-readable outcome of the upgrade attempt. |

### `GetBootstrapRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory to bootstrap against; empty means the process default. |

### `Bootstrap`

One-round-trip startup snapshot for frontends.

| Field | Type | Description |
|---|---|---|
| `location` (1) | `LocationInfo` | Process identity (directory, hostname, pid, version). |
| `config` (2) | `google.protobuf.Struct` | Effective runtime configuration for the directory. |
| `agents` (3) | `repeated AgentSummary` | Available agents bound to this directory. |
| `models` (4) | `repeated ModelSummary` | Available models across providers. |
| `providers` (5) | `repeated ProviderSummary` | Provider catalog with per-provider auth status. |
| `commands` (6) | `repeated CommandSummary` | Slash-command catalog. |
| `skills` (7) | `repeated SkillSummary` | Skill catalog. |
| `tools` (8) | `repeated ToolSummary` | Tool catalog including hidden aliases for completion UIs. |
| `interactions` (9) | `repeated Interaction` | Pending permission/question requests across sessions. |
| `saved_rules` (10) | `repeated SavedRule` | Saved permission rules. |
| `formatter_available` (11) | `bool` | Formatter availability advertised to the frontend. |
| `sessions_cursor` (12) | `string` | Cursor for the session list; fetch sessions with `Session.List`. |

### `ProjectInfo`

One registered project.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Project identifier. |
| `directory` (2) | `string` | Absolute directory of the project. |
| `name` (3) | `string` | Display name; defaults to the directory basename. |

### `ListProjectsRequest`


| Field | Type | Description |
|---|---|---|
| `page` (1) | `PageRequest` | Standard pagination controls. |

### `ListProjectsResponse`


| Field | Type | Description |
|---|---|---|
| `projects` (1) | `repeated ProjectInfo` | Known projects. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `UpdateProjectRequest`


| Field | Type | Description |
|---|---|---|
| `project` (1) | `string` | Project identifier. |
| `name` (2) | `optional string` | New display name when set. |

### `ListProjectDirectoriesRequest`


| Field | Type | Description |
|---|---|---|
| `project` (1) | `string` | Project identifier. |

### `ListProjectDirectoriesResponse`


| Field | Type | Description |
|---|---|---|
| `directories` (1) | `repeated string` | Absolute directory paths registered under the project. |

### `InitProjectGitRequest`


| Field | Type | Description |
|---|---|---|
| `project` (1) | `string` | Project identifier. |

### `InitProjectGitResponse`


| Field | Type | Description |
|---|---|---|
| `initialized` (1) | `bool` | Whether a new repository was initialized (false when one existed). |

### `GetVcsStatusRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |

### `VcsStatus`

Repository status snapshot.

| Field | Type | Description |
|---|---|---|
| `branch` (1) | `string` | Current branch name; empty in detached HEAD. |
| `head` (2) | `string` | Current HEAD commit hash. |
| `dirty` (3) | `uint32` | Number of uncommitted changes. |
| `ahead` (4) | `uint32` | Commits ahead of the upstream when known. |
| `behind` (5) | `uint32` | Commits behind the upstream when known. |
| `files` (6) | `repeated VcsFileChange` | Changed files with their status. |

### `VcsFileChange`

One changed file.

| Field | Type | Description |
|---|---|---|
| `path` (1) | `string` | Repository-relative path. |
| `status` (2) | `VcsFileStatus` | Change status. |

### `GetVcsDiffRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `raw` (2) | `bool` | Output format: unified patch (default) or raw. |
| `paths` (3) | `repeated string` | Restrict to these repository-relative paths; empty diffs everything. |

### `GetVcsDiffResponse`


| Field | Type | Description |
|---|---|---|
| `diff` (1) | `string` | Diff payload in the requested format. |

### `ApplyPatchRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `patch` (2) | `string` | Unified diff patch to apply. |

### `ApplyPatchResponse`


| Field | Type | Description |
|---|---|---|
| `applied` (1) | `bool` | Whether the patch applied cleanly. |
| `summary` (2) | `string` | Human-readable summary of the applied hunks. |

### `PtySession`

State snapshot of one PTY session.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | PTY session identifier. |
| `shell` (2) | `string` | Shell binary the session runs. |
| `cols` (3) | `uint32` | Current terminal width in columns. |
| `rows` (4) | `uint32` | Current terminal height in rows. |
| `cwd` (5) | `string` | Working directory the session started in. |

### `ListShellsResponse`


| Field | Type | Description |
|---|---|---|
| `shells` (1) | `repeated string` | Absolute paths of available shell binaries. |

### `CreatePtyRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; empty means the process default directory. |
| `shell` (2) | `string` | Shell binary name or path; empty picks the default shell. |
| `cols` (3) | `uint32` | Initial terminal width in columns. |
| `rows` (4) | `uint32` | Initial terminal height in rows. |
| `cwd` (5) | `string` | Working directory for the shell; defaults to the scope directory. |
| `string> env` (6) | `map<string,` | Extra environment variables for the shell process. |

### `GetPtyRequest`


| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | PTY session identifier. |

### `UpdatePtyRequest`


| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | PTY session identifier. |
| `cols` (2) | `optional uint32` | New terminal width in columns when resizing. |
| `rows` (3) | `optional uint32` | New terminal height in rows when resizing. |

### `DeletePtyRequest`


| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | PTY session identifier. |

### `CreateConnectTokenRequest`


| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | PTY session identifier. |

### `CreateConnectTokenResponse`


| Field | Type | Description |
|---|---|---|
| `token` (1) | `string` | One-time connection token. |
| `url` (2) | `string` | WebSocket URL to connect to with the token. |

### `PtyClientFrame`

Client-to-server terminal frame.

| Field | Type | Description |
|---|---|---|
| `attach` (4) | `oneof `frame`: PtyAttach` | Frame payload; exactly one kind is set. First frame on the gRPC `StreamPty` rpc: which session to attach to. |
| `input` (1) | `oneof `frame`: bytes` | Terminal input bytes (keystrokes, paste). |
| `resize` (2) | `oneof `frame`: PtyResize` | Terminal resize. |
| `ping` (3) | `oneof `frame`: bool` | Liveness ping. |

### `PtyAttach`

Session attachment envelope for the gRPC terminal stream.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | PTY session identifier to attach. |
| `token` (2) | `string` | Optional one-time connect token. |

### `PtyResize`

Terminal resize request.

| Field | Type | Description |
|---|---|---|
| `cols` (1) | `uint32` | New width in columns. |
| `rows` (2) | `uint32` | New height in rows. |

### `PtyServerFrame`

Server-to-client terminal frame.

| Field | Type | Description |
|---|---|---|
| `output` (1) | `oneof `frame`: bytes` | Frame payload; exactly one kind is set. Terminal output bytes. |
| `exit` (2) | `oneof `frame`: int32` | Session exit with the shell's exit code. |
| `pong` (3) | `oneof `frame`: bool` | Liveness pong. |

### `SessionRef`

Session id string (`hysec_...`, `ses_...`, or legacy raw UUID accepted on
input; canonical form on output).

| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |

### `SessionInfo`

Projection summary of one session.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Canonical session id. |
| `parent` (2) | `string` | Parent session id when this is a subagent/team child. |
| `title` (3) | `string` | Human-editable title; empty when unset. |
| `agent` (4) | `string` | Agent name bound to this session. |
| `model` (5) | `ModelRef` | Model the session currently runs on. |
| `workdir` (6) | `string` | Absolute working directory of the session. |
| `background` (7) | `bool` | Whether the session is flagged as background work. |
| `time_created` (8) | `google.protobuf.Timestamp` | When the session was created. |
| `time_updated` (9) | `google.protobuf.Timestamp` | When the session projection last changed. |
| `last_seq` (10) | `uint64` | Highest event sequence number recorded for this session. |
| `busy` (11) | `bool` | Whether a run currently owns the session's admission slot (derived from the process run registry, not the durable log). |

### `CreateSessionRequest`


| Field | Type | Description |
|---|---|---|
| `agent` (1) | `string` | Agent name or catalog id to bind as the session's default agent. |
| `model` (2) | `string` | Model reference the session starts on (`provider/model[#variant]`). |
| `workdir` (3) | `string` | Absolute workdir for tools and relative paths in this session. |
| `parent` (4) | `string` | When set, marks the new session as a child of this parent id. |
| `initialize` (5) | `bool` | When true, run the directory initialization turn after creation. |
| `title` (6) | `string` | Initial title; empty lets the backend derive one. |

### `CreateSessionResponse`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `SessionInfo` | Projection summary of the new session. |

### `GetSessionRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |

### `ListSessionsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory whose sessions should be listed. |
| `parent` (2) | `string` | Restrict to direct children of this session id when non-empty. |
| `page` (3) | `PageRequest` | Standard pagination controls. |

### `ListSessionsResponse`


| Field | Type | Description |
|---|---|---|
| `sessions` (1) | `repeated SessionInfo` | Session summaries in reverse-creation order. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `UpdateSessionRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |
| `title` (2) | `optional string` | New title when set. |
| `model` (3) | `optional string` | New model reference string when set. |
| `agent` (4) | `optional string` | New agent name when set. |
| `background` (5) | `optional bool` | New background flag when set. |

### `DeleteSessionRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |

### `ForkSessionRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier to fork from. |
| `until_seq` (2) | `uint64` | Copy events up to this sequence number; 0 forks at the current head. |

### `ForkSessionResponse`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `SessionInfo` | Projection summary of the forked session. |

### `CompactSessionRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier to compact. |
| `until_seq` (2) | `uint64` | Compact events up to this sequence number; 0 compacts at the head. |

### `CompactSessionResponse`


| Field | Type | Description |
|---|---|---|
| `compacted_until_seq` (1) | `uint64` | Watermark the context was compacted up to. |
| `strategy` (2) | `string` | Which compaction method fired (`shake`, `remote`, `soft`, `snap_compact`, `handoff`). |

### `SummarizeSessionRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier to summarize. |

### `SummarizeSessionResponse`


| Field | Type | Description |
|---|---|---|
| `summary_message` (1) | `string` | Id of the generated summary message. |

### `RevertSessionRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier to revert. |
| `until_seq` (2) | `uint64` | Revert target sequence number; 0 uses the last revert point. |
| `undo` (3) | `bool` | When true, undo the previous revert instead of reverting. |

### `RevertSessionResponse`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `SessionInfo` | Projection summary after the revert. |

### `PromptTurn`

A plain user prompt turn.

| Field | Type | Description |
|---|---|---|
| `text` (1) | `string` | User text recorded as the next user message. |

### `CommandTurn`

A slash-command turn.

| Field | Type | Description |
|---|---|---|
| `command` (1) | `string` | Command name without the leading `/` (for example `compact`). |
| `arguments` (2) | `string` | Raw argument string after the command name. |
| `text` (3) | `string` | Full composed text to store when the client already rendered the message body; empty lets the backend compose it. |
| `model` (4) | `string` | Model override for this turn (`provider/model[#variant]`). |

### `ShellTurn`

A synthetic shell turn: the command runs via the builtin shell tool with
no model round.

| Field | Type | Description |
|---|---|---|
| `command` (1) | `string` | Shell command line to execute. |
| `agent` (2) | `string` | Agent name used for message attribution. |
| `model` (3) | `ModelRef` | Client model selection retained for session continuity. |

### `CreateTurnRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier to admit the turn into. |
| `prompt` (2) | `oneof `kind`: PromptTurn` | Turn payload; exactly one kind is set. Admit a user prompt. |
| `command` (3) | `oneof `kind`: CommandTurn` | Admit a slash command. |
| `shell` (4) | `oneof `kind`: ShellTurn` | Admit a direct shell execution. |

### `CreateTurnResponse`


| Field | Type | Description |
|---|---|---|
| `turn` (1) | `TurnInfo` | Handle for the admitted turn. |

### `TurnInfo`

Projection snapshot of one admitted turn. The turn id is the id of the
assistant message the engine drives for that turn.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Turn identifier (assistant message id). |
| `session` (2) | `string` | Owning session identifier. |
| `state` (3) | `TurnState` | Current lifecycle state. |
| `finish` (4) | `FinishReason` | Terminal finish reason once state is FINISHED. |
| `error_code` (5) | `string` | Stable error code when state is FAILED. |
| `error_message` (6) | `string` | Human-readable error message when state is FAILED. |

### `GetTurnRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |
| `turn` (2) | `string` | Turn identifier. |

### `WaitTurnRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |
| `turn` (2) | `string` | Turn identifier. |
| `timeout_ms` (3) | `uint64` | Maximum time to wait in milliseconds; 0 waits indefinitely. |

### `CancelTurnRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |
| `turn` (2) | `string` | Turn identifier. |

### `WorkflowSummary`

One discovered workflow source.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Declared workflow name. |
| `revision` (2) | `string` | Current compiler revision of the source. |
| `description` (3) | `string` | One-line description from the source frontmatter. |
| `stage_count` (4) | `uint32` | Number of stages in the compiled plan. |

### `ListWorkflowsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory whose workflow sources should be listed. |
| `page` (2) | `PageRequest` | Standard pagination controls. |

### `ListWorkflowsResponse`


| Field | Type | Description |
|---|---|---|
| `workflows` (1) | `repeated WorkflowSummary` | Discovered workflow sources. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `GetWorkflowStateRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |

### `WorkflowStageRun`

Execution state of one stage member.

| Field | Type | Description |
|---|---|---|
| `stage` (1) | `string` | Stage name from the compiled plan. |
| `member` (2) | `string` | Member/session id executing the stage when spawned. |
| `agent` (3) | `string` | Agent name bound to the stage. |
| `status` (4) | `WorkflowRunStatus` | Stage lifecycle status. |

### `WorkflowState`

Projected workflow state of a session.

| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Owning session identifier. |
| `workflow` (2) | `string` | Selected workflow name; empty when none is selected. |
| `revision` (3) | `string` | Selected compiler revision. |
| `status` (4) | `WorkflowRunStatus` | Aggregate run status. |
| `stages` (5) | `repeated WorkflowStageRun` | Stage execution rows in plan order. |
| `error_code` (6) | `string` | Terminal failure code when status is FAILED. |
| `raw_json` (7) | `string` | Opaque canonical projection JSON for tooling and replay parity. The internal shape is not a stable contract; prefer the typed fields. |

### `WorkflowInfoCommand`

`list` command: enumerate workflow sources.
`info` command: inspect one compiled workflow.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Declared workflow name. |

### `WorkflowSelectCommand`

`select` command: persist one source/revision identity.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Declared workflow name. |
| `expected_revision` (2) | `string` | Optional optimistic compiler revision to match. |

### `WorkflowRunCommand`

`run` command: start the selected or named workflow.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Declared workflow name; empty uses the durable selection. |
| `inputs` (2) | `google.protobuf.Struct` | Workflow inputs as a JSON object. |

### `SubmitWorkflowCommandRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier the command applies to. |
| `list` (2) | `oneof `command`: WorkflowListCommand` | Command payload; exactly one kind is set. List workflow sources. |
| `info` (3) | `oneof `command`: WorkflowInfoCommand` | Inspect one compiled workflow. |
| `select` (4) | `oneof `command`: WorkflowSelectCommand` | Select a workflow source. |
| `run` (5) | `oneof `command`: WorkflowRunCommand` | Run the selected workflow. |

### `WorkflowModelCandidate`

One authored fallback candidate.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Base model identity (`provider/model`). |
| `reasoning` (2) | `string` | Optional author-provided effort label. |

### `WorkflowModelAssignment`

Authored worker model assignment for a stage.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Preferred base model identity (`provider/model`). |
| `reasoning` (2) | `string` | Optional preferred effort label. |
| `fallback` (3) | `repeated WorkflowModelCandidate` | Ordered fallback tail. |

### `WorkflowInfoResult`

Result payload of the `info` command.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Compiled workflow name. |
| `revision` (2) | `string` | Compiler revision of this graph. |
| `stage_names` (3) | `repeated string` | Stage names in execution order. |
| `stages` (4) | `repeated WorkflowStageInfo` | Stage metadata in execution order. |

### `WorkflowStageInfo`

Compiled stage metadata from the `info` result.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Compiled stage id. |
| `agent` (2) | `string` | Target agent id. |
| `level` (3) | `uint32` | Zero-based topological level. |
| `worker_model` (4) | `WorkflowModelAssignment` | Authored worker model assignment when present. |
| `verifier_model` (5) | `WorkflowModelAssignment` | Authored verifier model assignment when present. |

### `SubmitWorkflowCommandResponse`


| Field | Type | Description |
|---|---|---|
| `list` (1) | `oneof `result`: ListWorkflowsResponse` | Command outcome; exactly one kind is set. Rows from the `list` command. |
| `info` (2) | `oneof `result`: WorkflowInfoResult` | Compiled graph from the `info` command. |
| `selected` (3) | `oneof `result`: WorkflowState` | State after the `select` command. |
| `started` (4) | `oneof `result`: WorkflowState` | State after admitting the `run` command (status becomes RUNNING). |

### `Worktree`

One git worktree.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Worktree identifier. |
| `path` (2) | `string` | Absolute path of the worktree directory. |
| `branch` (3) | `string` | Branch checked out in the worktree. |
| `head` (4) | `string` | HEAD commit hash when known. |

### `ListWorktreesRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope of the owning repository; empty means the process default directory. |

### `ListWorktreesResponse`


| Field | Type | Description |
|---|---|---|
| `worktrees` (1) | `repeated Worktree` | Worktrees of the repository. |

### `CreateWorktreeRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope of the owning repository; empty means the process default directory. |
| `name` (2) | `string` | Worktree name; derived from the branch when empty. |
| `branch` (3) | `string` | Branch to check out; created from the current HEAD when empty. |

### `DeleteWorktreeRequest`


| Field | Type | Description |
|---|---|---|
| `worktree` (1) | `string` | Worktree identifier to delete. |
| `delete_branch` (2) | `bool` | Also delete the checked-out branch. |

### `ResetWorktreeRequest`


| Field | Type | Description |
|---|---|---|
| `worktree` (1) | `string` | Worktree identifier to reset. |

## Enums

### `AuthStatus`

Authentication state of a provider route.

| Value | Number | Description |
|---|---|---|
| `AUTH_STATUS_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `AUTH_STATUS_CREDENTIALED` | 1 | Credentials present and accepted. |
| `AUTH_STATUS_UNAUTHENTICATED` | 2 | Route needs no credentials. |
| `AUTH_STATUS_AUTH_REQUIRED` | 3 | Credentials missing; provider requires them. |
| `AUTH_STATUS_AUTH_REJECTED` | 4 | Credentials present but rejected by the provider. |
| `AUTH_STATUS_NOT_APPLICABLE` | 5 | Auth does not apply to this route kind. |

### `DirEntryKind`

Kind of a directory entry.

| Value | Number | Description |
|---|---|---|
| `DIR_ENTRY_KIND_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `DIR_ENTRY_KIND_FILE` | 1 | A regular file. |
| `DIR_ENTRY_KIND_DIRECTORY` | 2 | A subdirectory. |
| `DIR_ENTRY_KIND_SYMLINK` | 3 | A symbolic link. |

### `SymbolKind`

Kind of a discovered symbol.

| Value | Number | Description |
|---|---|---|
| `SYMBOL_KIND_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `SYMBOL_KIND_FUNCTION` | 1 | A function definition. |
| `SYMBOL_KIND_TYPE` | 2 | A type, struct, or class definition. |
| `SYMBOL_KIND_MODULE` | 3 | A module or namespace. |
| `SYMBOL_KIND_OTHER` | 4 | Anything else. |

### `InteractionType`

Kind of a pending interaction.

| Value | Number | Description |
|---|---|---|
| `INTERACTION_TYPE_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `INTERACTION_TYPE_PERMISSION` | 1 | A tool permission decision. |
| `INTERACTION_TYPE_QUESTION` | 2 | A question the engine asks the user. |

### `RulePermission`

Effect of a saved permission rule.

| Value | Number | Description |
|---|---|---|
| `RULE_PERMISSION_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `RULE_PERMISSION_ALLOW` | 1 | Allow matching calls without asking. |
| `RULE_PERMISSION_ASK` | 2 | Ask the user for matching calls. |
| `RULE_PERMISSION_DENY` | 3 | Deny matching calls without asking. |

### `LogLevel`

Severity of an ingested log entry.

| Value | Number | Description |
|---|---|---|
| `LOG_LEVEL_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `LOG_LEVEL_DEBUG` | 1 | Debug-level diagnostics. |
| `LOG_LEVEL_INFO` | 2 | Informational entries. |
| `LOG_LEVEL_WARN` | 3 | Warnings. |
| `LOG_LEVEL_ERROR` | 4 | Errors. |

### `McpServerState`

Connection state of an MCP server.

| Value | Number | Description |
|---|---|---|
| `MCP_SERVER_STATE_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `MCP_SERVER_STATE_DESIRED` | 1 | Present in desired state but not yet connected. |
| `MCP_SERVER_STATE_CONNECTED` | 2 | Transport established and tools listed. |
| `MCP_SERVER_STATE_DISCONNECTED` | 3 | Deliberately disconnected. |
| `MCP_SERVER_STATE_FAILED` | 4 | Connection or protocol failure. |

### `FinishReason`

Terminal reason of an assistant message.

| Value | Number | Description |
|---|---|---|
| `FINISH_REASON_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `FINISH_REASON_STOP` | 1 | Normal completion with no further tool calls. |
| `FINISH_REASON_TOOL_CALLS` | 2 | Model requested tools; the turn continues with another round. |
| `FINISH_REASON_LENGTH` | 3 | Hit an output length limit. |
| `FINISH_REASON_CANCELLED` | 4 | Cancel token, sidecar loss, or client abort. |
| `FINISH_REASON_ERROR` | 5 | Hard provider/tool failure after the assistant message started. |

### `Role`

Author role of a message.

| Value | Number | Description |
|---|---|---|
| `ROLE_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `ROLE_USER` | 1 | Human user message. |
| `ROLE_ASSISTANT` | 2 | Model-generated message. |
| `ROLE_SYSTEM` | 3 | Engine-injected system message. |
| `ROLE_TOOL` | 4 | Synthetic tool-authored message (for example shell turns). |

### `ToolExecutionState`

Execution state of one tool call.

| Value | Number | Description |
|---|---|---|
| `TOOL_EXECUTION_STATE_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `TOOL_EXECUTION_STATE_PENDING` | 1 | Waiting for admission or permission. |
| `TOOL_EXECUTION_STATE_RUNNING` | 2 | Currently executing. |
| `TOOL_EXECUTION_STATE_OK` | 3 | Completed successfully. |
| `TOOL_EXECUTION_STATE_ERROR` | 4 | Failed with an error. |
| `TOOL_EXECUTION_STATE_DENIED` | 5 | Rejected by policy or the user. |

### `TodoStatus`

Lifecycle status of a todo item.

| Value | Number | Description |
|---|---|---|
| `TODO_STATUS_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `TODO_STATUS_PENDING` | 1 | Not started. |
| `TODO_STATUS_IN_PROGRESS` | 2 | Currently being worked on. |
| `TODO_STATUS_COMPLETED` | 3 | Done. |

### `VcsFileStatus`

Change status of one file.

| Value | Number | Description |
|---|---|---|
| `VCS_FILE_STATUS_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `VCS_FILE_STATUS_ADDED` | 1 | Staged or unstaged addition. |
| `VCS_FILE_STATUS_MODIFIED` | 2 | Content modification. |
| `VCS_FILE_STATUS_DELETED` | 3 | Deletion. |
| `VCS_FILE_STATUS_RENAMED` | 4 | Rename. |
| `VCS_FILE_STATUS_UNTRACKED` | 5 | Not tracked by VCS. |

### `TurnState`

Terminal state of a turn.

| Value | Number | Description |
|---|---|---|
| `TURN_STATE_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `TURN_STATE_ADMITTED` | 1 | Admitted and queued, not yet running. |
| `TURN_STATE_RUNNING` | 2 | Model/tool rounds in flight. |
| `TURN_STATE_FINISHED` | 3 | Terminal success. |
| `TURN_STATE_FAILED` | 4 | Terminal failure. |
| `TURN_STATE_CANCELLED` | 5 | Cancelled by the client or a stop signal. |

### `WorkflowRunStatus`

Lifecycle status of a workflow run.

| Value | Number | Description |
|---|---|---|
| `WORKFLOW_RUN_STATUS_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `WORKFLOW_RUN_STATUS_SELECTED` | 1 | Selected but not started. |
| `WORKFLOW_RUN_STATUS_RUNNING` | 2 | Stages are executing. |
| `WORKFLOW_RUN_STATUS_FINISHED` | 3 | All stages completed successfully. |
| `WORKFLOW_RUN_STATUS_FAILED` | 4 | A stage failed terminally. |
| `WORKFLOW_RUN_STATUS_CANCELLED` | 5 | Cancelled by command or shutdown. |

