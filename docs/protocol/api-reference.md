# hya API v1 Reference

Generated from `proto/hya/v1` by `cargo xtask gen-api`; do not edit by hand.
The same contract is served over HTTP/JSON+SSE and gRPC. Every rpc lists its
HTTP binding (`method path`) and its fully-qualified gRPC method
(`hya.v1.<Service>.<Rpc>`).

Conventions: pagination uses opaque cursors; errors use the stable code
table (`hya_api::error`); timestamps are RFC 3339 strings in JSON. An `ANY`
binding accepts GET, POST, PUT, PATCH, and DELETE on one path (the request
message names the method), and its trailing `{path}` spans every remaining
path segment.

## Contents

- [AgentModels service](#service-agentmodels)
- [Auth service](#service-auth)
- [BundleApi service](#service-bundleapi)
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

## Service `AgentModels`

Durable per-agent model preference surface, backed by the app-owned
control handle.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListAgentModels` | `GET /v1/agent-models` | `hya.v1.AgentModels.ListAgentModels` | `ListAgentModelsRequest` | `ListAgentModelsResponse` |
| `SetAgentModel` | `PUT /v1/agent-models/{agent_id}` | `hya.v1.AgentModels.SetAgentModel` | `SetAgentModelRequest` | `AgentModelState` |

### `AgentModels.ListAgentModels`

Effective model state for every catalog agent under one binding.


### `AgentModels.SetAgentModel`

Set or clear one agent's remembered preference; returns the
post-commit state.


## Service `Auth`

Credential surface for model providers. This is hya's real auth system
(per-provider credentials in the user auth directory); it does not cover
third-party service connectors, which are out of scope for v1.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListProviderAuth` | `GET /v1/auth` | `hya.v1.Auth.ListProviderAuth` | `ListProviderAuthRequest` | `ListProviderAuthResponse` |
| `SetProviderAuth` | `PUT /v1/auth/{provider_id}` | `hya.v1.Auth.SetProviderAuth` | `SetProviderAuthRequest` | `SetProviderAuthResponse` |
| `RemoveProviderAuth` | `DELETE /v1/auth/{provider_id}` | `hya.v1.Auth.RemoveProviderAuth` | `RemoveProviderAuthRequest` | `RemoveProviderAuthResponse` |
| `StartOauth` | `POST /v1/auth/{provider_id}/oauth/start` | `hya.v1.Auth.StartOauth` | `StartOauthRequest` | `StartOauthResponse` |
| `CompleteOauth` | `POST /v1/auth/{provider_id}/oauth/callback` | `hya.v1.Auth.CompleteOauth` | `CompleteOauthRequest` | `CompleteOauthResponse` |

### `Auth.ListProviderAuth`

List provider ids with saved credentials. Never returns secret values.


### `Auth.SetProviderAuth`

Store an API key (written atomically with mode 0600 as `type: api`) and
rebuild that provider's route live; a provider with no cached remote
models also fetches its model list.


### `Auth.RemoveProviderAuth`

Delete the stored credentials for a provider and rebuild its route live
(an inline config `api_key`, if any, applies again).


### `Auth.StartOauth`

Begin a provider OAuth flow; returns the authorization URL to open.


### `Auth.CompleteOauth`

Complete a provider OAuth flow with the callback code.


## Service `BundleApi`

Endpoints that installed bundles register for themselves (manifest
`apis:`), answered by the bundle's `extensions.process` over the plugin
`api/request` request. A session-scoped endpoint is mounted under one
session and its process may read that session's data through a
request-scoped read-only capability; a global endpoint is not tied to any
session. Write methods only change state the bundle process owns itself.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListBundleApis` | `GET /v1/bundle-apis` | `hya.v1.BundleApi.ListBundleApis` | `ListBundleApisRequest` | `ListBundleApisResponse` |
| `InvokeSessionBundleApi` | `ANY /v1/sessions/{session}/bundles/{bundle}/{path}` | `hya.v1.BundleApi.InvokeSessionBundleApi` | `InvokeSessionBundleApiRequest` | `BundleApiResponse` |
| `InvokeGlobalBundleApi` | `ANY /v1/bundles/{bundle}/api/{path}` | `hya.v1.BundleApi.InvokeGlobalBundleApi` | `InvokeGlobalBundleApiRequest` | `BundleApiResponse` |

### `BundleApi.ListBundleApis`

List every endpoint the published bundles register, sorted by bundle id
then endpoint id.


### `BundleApi.InvokeSessionBundleApi`

Invoke one session-scoped bundle endpoint. Over HTTP `bundle` is one
percent-encoded path segment (`hya-extra%2Ftoken-summary`), `{path}` is
the rest of the URL path (matched against the bundle's templates), the
query string becomes `query`, a non-empty request body must be JSON, and
the response is the process's own status and JSON body verbatim (no
envelope). Over gRPC the reply is `BundleApiResponse`, whose `status`
carries the process status (a non-2xx process status is still an OK
gRPC call); only host-side failures are gRPC errors.


### `BundleApi.InvokeGlobalBundleApi`

Invoke one global bundle endpoint (not tied to a session); otherwise
identical to `InvokeSessionBundleApi`.


## Service `Catalog`

Catalog surface used by pickers, completion UIs, and setup flows, plus
the provider-management calls behind the TUI Provider View (add or update
a provider, refresh its remote model list, override model metadata in
`config.yaml`, and test a model). Provider changes apply live: the server
rebuilds that provider's route and the catalog, and emits
`catalog.updated`.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListAgents` | `GET /v1/agents` | `hya.v1.Catalog.ListAgents` | `ListAgentsRequest` | `ListAgentsResponse` |
| `ListModels` | `GET /v1/models` | `hya.v1.Catalog.ListModels` | `ListModelsRequest` | `ListModelsResponse` |
| `ListProviders` | `GET /v1/providers` | `hya.v1.Catalog.ListProviders` | `ListProvidersRequest` | `ListProvidersResponse` |
| `GetProvider` | `GET /v1/providers/{provider_id}` | `hya.v1.Catalog.GetProvider` | `GetProviderRequest` | `ProviderInfo` |
| `UpsertProvider` | `PUT /v1/providers/{provider_id}` | `hya.v1.Catalog.UpsertProvider` | `UpsertProviderRequest` | `ProviderUpdate` |
| `RefreshProvider` | `POST /v1/providers/{provider_id}/refresh` | `hya.v1.Catalog.RefreshProvider` | `RefreshProviderRequest` | `ProviderUpdate` |
| `SetProviderModel` | `PUT /v1/providers/{provider_id}/models` | `hya.v1.Catalog.SetProviderModel` | `SetProviderModelRequest` | `ProviderUpdate` |
| `RemoveProviderModel` | `DELETE /v1/providers/{provider_id}/models` | `hya.v1.Catalog.RemoveProviderModel` | `RemoveProviderModelRequest` | `ProviderUpdate` |
| `TestProviderModel` | `POST /v1/providers/{provider_id}/test` | `hya.v1.Catalog.TestProviderModel` | `TestProviderModelRequest` | `TestProviderModelResponse` |
| `ListCommands` | `GET /v1/commands` | `hya.v1.Catalog.ListCommands` | `ListCommandsRequest` | `ListCommandsResponse` |
| `ListSkills` | `GET /v1/skills` | `hya.v1.Catalog.ListSkills` | `ListSkillsRequest` | `ListSkillsResponse` |
| `ListTools` | `GET /v1/tools` | `hya.v1.Catalog.ListTools` | `ListToolsRequest` | `ListToolsResponse` |
| `ListPermissionModes` | `GET /v1/permission-modes` | `hya.v1.Catalog.ListPermissionModes` | `ListPermissionModesRequest` | `ListPermissionModesResponse` |

### `Catalog.ListAgents`

Agents that can be bound to a session.


### `Catalog.ListModels`

Models across providers, optionally filtered to one provider.


### `Catalog.ListProviders`

Providers with their aggregate auth status.


### `Catalog.GetProvider`

One provider's detail including its models.


### `Catalog.UpsertProvider`

Add or update a provider in `config.yaml` (and save its API key when
one is given), fetch its remote model list into the model cache, and
apply it live. A failed fetch does not fail the call: `discovery`
reports it.


### `Catalog.RefreshProvider`

Re-read the provider's config entry and key, fetch its remote model
list into the model cache, and apply it live.


### `Catalog.SetProviderModel`

Write one model's entry (with its metadata overrides) into the
provider's `models:` in `config.yaml` and apply it live. The model id
travels in the body because ids may contain `/` or `:`.


### `Catalog.RemoveProviderModel`

Remove one model's entry from the provider's `models:` in
`config.yaml` and apply it live; a remote model stays listed from the
model cache. The model id travels as the `modelId` query parameter.


### `Catalog.TestProviderModel`

Send one `hi` user message to a provider's model with max output tokens
1 (no tools, no reasoning) and report whether a normal reply came back.


### `Catalog.ListCommands`

Slash-command catalog entries.


### `Catalog.ListSkills`

Skill catalog entries.


### `Catalog.ListTools`

Tool registry entries including hidden aliases.


### `Catalog.ListPermissionModes`

Session permission modes accepted by `UpdateSession.permission_mode`:
the built-in `manual` and `yolo`, then every installed bundle's
`permission_modes:` as `<bundle-id>/<mode-id>`.


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
HTTP. Delivers durable events and live-only (`seq = 0`) frames as they
happen; it does not replay history (read `ListEvents` for that). Emits
`resync` when the consumer lags: frames in the gap (live deltas
included) are lost, so re-read the projection (`ListMessages`) or
replay `ListEvents` from the last durable `seq` applied.


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

Add (or replace) one MCP server in desired state. The server is stored
even when it cannot start: a spawn or handshake failure is answered
with `state: MCP_SERVER_STATE_FAILED` and `error` (the call succeeds).
With `enabled: false` the server is stored without connecting
(`MCP_SERVER_STATE_DISCONNECTED`). Re-adding an unchanged config does
not reconnect.


### `Mcp.ConnectMcp`

Enable and connect one MCP server now. A connect failure is answered
with `state: MCP_SERVER_STATE_FAILED` and `error`; an unknown name is
`not_found`.


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

Project and VCS surface. A Project (ADR-0024) is a named, ordered,
non-empty list of absolute root directories on the backend machine; the
first root is the primary root. Sessions belong to at most one Project.

| RPC | HTTP | gRPC | Request | Response |
|---|---|---|---|---|
| `ListProjects` | `GET /v1/projects` | `hya.v1.Project.ListProjects` | `ListProjectsRequest` | `ListProjectsResponse` |
| `GetCurrentProject` | `GET /v1/projects/current` | `hya.v1.Project.GetCurrentProject` | `GetCurrentProjectRequest` | `ProjectInfo` |
| `ResolveProject` | `GET /v1/projects/resolve` | `hya.v1.Project.ResolveProject` | `ResolveProjectRequest` | `ResolveProjectResponse` |
| `EnsureProjectForPath` | `POST /v1/projects/ensure` | `hya.v1.Project.EnsureProjectForPath` | `EnsureProjectForPathRequest` | `EnsureProjectForPathResponse` |
| `CreateProject` | `POST /v1/projects` | `hya.v1.Project.CreateProject` | `CreateProjectRequest` | `ProjectInfo` |
| `GetProject` | `GET /v1/projects/{project}` | `hya.v1.Project.GetProject` | `GetProjectRequest` | `ProjectInfo` |
| `UpdateProject` | `PATCH /v1/projects/{project}` | `hya.v1.Project.UpdateProject` | `UpdateProjectRequest` | `ProjectInfo` |
| `DeleteProject` | `DELETE /v1/projects/{project}` | `hya.v1.Project.DeleteProject` | `DeleteProjectRequest` | `DeleteProjectResponse` |
| `ListProjectDirectories` | `GET /v1/projects/{project}/directories` | `hya.v1.Project.ListProjectDirectories` | `ListProjectDirectoriesRequest` | `ListProjectDirectoriesResponse` |
| `InitProjectGit` | `POST /v1/projects/{project}/init-git` | `hya.v1.Project.InitProjectGit` | `InitProjectGitRequest` | `InitProjectGitResponse` |
| `GetVcsStatus` | `GET /v1/vcs` | `hya.v1.Project.GetVcsStatus` | `GetVcsStatusRequest` | `VcsStatus` |
| `GetVcsDiff` | `GET /v1/vcs/diff` | `hya.v1.Project.GetVcsDiff` | `GetVcsDiffRequest` | `GetVcsDiffResponse` |
| `ApplyPatch` | `POST /v1/vcs/apply` | `hya.v1.Project.ApplyPatch` | `ApplyPatchRequest` | `ApplyPatchResponse` |

### `Project.ListProjects`

List the Projects (archived ones are left out), most recently updated
first.


### `Project.GetCurrentProject`

The Project whose root contains the request's directory scope (the
`x-hya-directory` header, else `directory`), matched like
`ResolveProject`. `invalid_argument` without a scope or for a relative
scope; `not_found` when no Project contains it.


### `Project.ResolveProject`

The Project that contains `path`, without creating one: `project` is
unset when none does. A path inside several roots matches the longest
root. `invalid_argument` for a relative path or one with a `..`
component.


### `Project.EnsureProjectForPath`

The Project for a local working directory: the Project that contains
`path` (as `ResolveProject`), else a new Project named after the last
component of `path` whose only root is `path`. `created` tells which.
`invalid_argument` as for `ResolveProject`.


### `Project.CreateProject`

Create a Project. `invalid_argument` for an empty name, no roots, or a
relative root or one with a `..` component. Roots are normalized (`.`
components and trailing separators dropped) and de-duplicated; they need
not exist.


### `Project.GetProject`

Read one Project. `not_found` when it does not exist.


### `Project.UpdateProject`

Rename a Project and/or replace its roots, in one step. Running
sessions of the Project see new roots from their next turn.
`not_found` when it does not exist; `invalid_argument` as for
`CreateProject`.


### `Project.DeleteProject`

Delete a Project. `failed_precondition` while a non-archived root
session belongs to it; `not_found` when it does not exist. Archived and
deleted sessions keep the id, which then names no Project.


### `Project.ListProjectDirectories`

The roots of a Project, primary root first. `not_found` when it does
not exist.


### `Project.InitProjectGit`

Initialize git in the primary root of a Project that has no repository
yet. `not_found` when the Project does not exist.


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
Archived root sessions are left out unless `include_archived` or
`archived_only` is set.


### `Session.UpdateSession`

Update mutable session fields: title, agent, model, background flag,
permission mode, and the archived flag of a root session.


### `Session.DeleteSession`

Delete a session and its event log.


### `Session.ForkSession`

Fork a session into a new session id. The fork copies the source's
visible messages (never those hidden by a pending revert): all of them
(the head), or those before `message_id`, or those started at or before
`until_seq`. The new session records its source (`SessionInfo
.forked_from`).


### `Session.CompactSession`

Compact a session's context using the configured method ladder.


### `Session.SummarizeSession`

Produce a summary message for a session (titles, handoffs).


### `Session.RevertSession`

Revert a session to just before a user message (default: the last
one), or undo the pending revert (`undo`). A revert hides that user
message and every later message and restores the files their tool
calls changed; the next prompt or shell turn commits it. Fails with
`session_busy` while a turn runs.


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
direct shell execution. Returns as soon as the turn is admitted. There
is no server-side prompt queue: while another run owns the session the
call fails with `session_busy` (HTTP 409); clients queue follow-ups
themselves and submit after the assistant `messageFinished`.


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

### `AgentModelSelection`

A concrete provider/model selection.

| Field | Type | Description |
|---|---|---|
| `provider_id` (1) | `string` | Provider identifier. |
| `model_id` (2) | `string` | Provider-local model identifier. |

### `AgentModelState`

Effective model state for one catalog agent.

| Field | Type | Description |
|---|---|---|
| `agent_id` (1) | `string` | Stable catalog agent id. |
| `description` (2) | `string` | Human-readable agent description. |
| `mode` (3) | `string` | Selector role (`primary` or `subagent`). |
| `hidden` (4) | `bool` | Whether the agent is hidden from ordinary selection. |
| `configured` (5) | `bool` | Whether direct model/category configuration is present (such agents cannot take a remembered preference). |
| `settable` (6) | `bool` | Whether an automatic remembered preference can be set. |
| `preference` (7) | `AgentModelSelection` | Retained preference, including stale or configured rows. |
| `preference_available` (8) | `bool` | Whether the retained preference exactly matches the current catalog. |
| `effective` (9) | `AgentModelSelection` | Current effective model identity. |
| `source` (10) | `AgentModelSource` | Which tier resolved the effective model. |
| `configuration` (11) | `AgentModelSelection` | Model explicitly stored in the owning user configuration file. |
| `session_override` (12) | `AgentModelSelection` | Active root-session override captured for this agent. |

### `ListAgentModelsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope for the agent binding (absolute). Optional: when both it and `session` are empty the global (project-less) binding is used. |
| `session` (2) | `string` | Bind against this session's runtime when non-empty (its workdir and session overrides); otherwise the directory root binding is used. |

### `ListAgentModelsResponse`


| Field | Type | Description |
|---|---|---|
| `agents` (1) | `repeated AgentModelState` | Effective state for every agent in the binding, stable id order. |

### `SetAgentModelRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope for the agent binding (absolute). Optional: when both it and `session` are empty the global (project-less) binding is used. |
| `session` (2) | `string` | Bind against this session's runtime when non-empty. |
| `agent_id` (3) | `string` | Stable catalog agent id whose preference is being set. |
| `preference` (4) | `optional AgentModelSelection` | New remembered preference; absent/null clears it. |

### `ListProviderAuthResponse`


| Field | Type | Description |
|---|---|---|
| `provider_ids` (1) | `repeated string` | Sorted provider ids that have a stored credential file. |

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
| `provider` (2) | `ProviderInfo` | The provider after the live rebuild; unset when the id is not a configured provider. |
| `discovery` (3) | `DiscoveryOutcome` | Remote model-list fetch outcome when the save triggered one. |

### `RemoveProviderAuthRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory context for auth resolution. |
| `provider_id` (2) | `string` | Provider identifier whose credentials should be deleted. |

### `RemoveProviderAuthResponse`


| Field | Type | Description |
|---|---|---|
| `provider` (1) | `ProviderInfo` | The provider after the live rebuild; unset when the id is not a configured provider. |

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

### `BundleApiInfo`

One endpoint a published bundle registers.

| Field | Type | Description |
|---|---|---|
| `bundle` (1) | `string` | Bundle identity id (for example `hya-extra/token-summary`). |
| `api` (2) | `string` | Endpoint id declared in the bundle manifest. |
| `method` (3) | `string` | HTTP method: `GET`, `POST`, `PUT`, `PATCH`, or `DELETE`. |
| `scope` (4) | `string` | Mount scope: `session` (`/v1/sessions/{session}/bundles/{bundle}/...`) or `global` (`/v1/bundles/{bundle}/api/...`). |
| `path` (5) | `string` | Path template below the mount, for example `/items/{id}`. |
| `description` (6) | `string` | Manifest description; empty when not declared. |
| `request_schema` (7) | `google.protobuf.Value` | JSON Schema of the request body; absent when not declared. |
| `response_schema` (8) | `google.protobuf.Value` | JSON Schema of the response body; absent when not declared. |

### `ListBundleApisResponse`


| Field | Type | Description |
|---|---|---|
| `apis` (1) | `repeated BundleApiInfo` | Endpoints sorted by bundle id, then endpoint id. |

### `InvokeSessionBundleApiRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session the endpoint is invoked for (must exist). |
| `bundle` (2) | `string` | Bundle identity id; contains `/`, so HTTP carries it percent-encoded as one path segment. |
| `method` (3) | `string` | HTTP method: `GET`, `POST`, `PUT`, `PATCH`, or `DELETE` (HTTP: the request method). |
| `path` (4) | `string` | Request path below the bundle mount with a leading `/` (for example `/items/42`); segments may be percent-encoded. |
| `string> query` (5) | `map<string,` | Query parameters passed to the process verbatim (HTTP: the query string). |
| `body` (6) | `google.protobuf.Value` | JSON request body; absent for none (HTTP: the request body, at most 512 KiB). |

### `InvokeGlobalBundleApiRequest`


| Field | Type | Description |
|---|---|---|
| `bundle` (1) | `string` | Bundle identity id (HTTP: one percent-encoded path segment). |
| `method` (2) | `string` | HTTP method: `GET`, `POST`, `PUT`, `PATCH`, or `DELETE`. |
| `path` (3) | `string` | Request path below the bundle mount with a leading `/`. |
| `string> query` (4) | `map<string,` | Query parameters passed to the process verbatim. |
| `body` (5) | `google.protobuf.Value` | JSON request body; absent for none (at most 512 KiB). |

### `BundleApiResponse`

One served bundle API call (the gRPC reply; HTTP returns `body` verbatim
with `status` as the HTTP status).

| Field | Type | Description |
|---|---|---|
| `bundle` (1) | `string` | Bundle identity id. |
| `api` (2) | `string` | Matched endpoint id. |
| `status` (3) | `uint32` | Status the bundle process answered, in 200..=599. |
| `content_type` (4) | `string` | Media type of `body`: `application/json`, or empty when there is no body. |
| `body` (5) | `google.protobuf.Value` | The process's JSON answer; absent when it answered no body. Numbers are doubles over gRPC (HTTP keeps integers exact). |

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
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Optional: empty lists the global view (no project sources). |
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
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
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
| `reasoning` (5) | `optional bool` | Whether the model supports reasoning effort variants, as declared by its metadata (the remote model list or the config `reasoning` field); unset when unknown. An unknown model still accepts the provider family's effort variants at runtime. |
| `auth` (6) | `AuthStatus` | Auth state of the owning provider route. |
| `context_limit` (7) | `uint64` | Context window in tokens from the model's metadata (config `limit.context`, else the remote model list); 0 (omitted) when unknown. With no known window the runtime sizes compaction against a 200000-token fallback. |
| `output_limit` (8) | `uint64` | Maximum output tokens from the model's metadata (config `limit.output`, else the remote model list); 0 (omitted) when unknown. |
| `source` (9) | `string` | Where the row comes from: `remote` (the provider's remote model list, via the model cache), `config` (only a `models:` entry in `config.yaml`), `override` (both; config fields win field by field), or `offline` (the built-in `hya/offline` row). |
| `image_input` (10) | `optional bool` | Whether the model accepts image input (config `modalities.input` contains `image`); unset when unknown. Prompt turns with attachments are refused only when this is `false`. |

### `ListModelsResponse`


| Field | Type | Description |
|---|---|---|
| `models` (1) | `repeated ModelSummary` | Models matching the filter. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `ListProvidersRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
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
| `kind` (6) | `string` | Config `kind` (`openai`, `openai-response`, `anthropic`, `google`, `openai-codex`, `grok-build`); empty for the offline provider. |
| `base_url` (7) | `string` | Config `base_url`; empty for the offline provider. |
| `key_source` (8) | `string` | Where the provider's credential comes from: `saved` (an API key in `auth/<id>.yaml`), `oauth` (a saved OAuth bundle), `config` (an inline `api_key` in `config.yaml`), or `none`. Never the secret itself. |
| `model_count` (9) | `uint32` | Number of model rows the provider currently serves. |

### `ListProvidersResponse`


| Field | Type | Description |
|---|---|---|
| `providers` (1) | `repeated ProviderSummary` | Providers visible in this directory. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `GetProviderRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
| `provider_id` (2) | `string` | Provider identifier. |

### `ProviderInfo`

Provider detail with its model rows.

| Field | Type | Description |
|---|---|---|
| `summary` (1) | `ProviderSummary` | Provider summary. |
| `models` (2) | `repeated ModelSummary` | Models exposed by this provider. |
| `supports_api_key` (3) | `bool` | Whether an API-key auth method is supported. |
| `supports_oauth` (4) | `bool` | Whether an OAuth flow is supported. |

### `UpsertProviderRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory context (unused; providers are process-wide). |
| `provider_id` (2) | `string` | Provider id: 1-64 ASCII letters, digits, `-`, or `_` (`hya` is reserved for the offline provider). |
| `kind` (3) | `string` | Protocol kind: `openai` (OpenAI-compatible Chat Completions), `openai-response`, `anthropic`, or `google`; the config aliases `openai-compatible`, `openai-completion`, `openai-codex`, and `grok-build` are accepted too. |
| `base_url` (4) | `string` | API root, `http://` or `https://` (for example `https://api.openai.com/v1`). |
| `api_key` (5) | `optional string` | API key to save in `auth/<id>.yaml`; absent or empty keeps the current credential. |

### `RefreshProviderRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory context (unused; providers are process-wide). |
| `provider_id` (2) | `string` | Configured provider id. |

### `SetProviderModelRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory context (unused; providers are process-wide). |
| `provider_id` (2) | `string` | Configured provider id. |
| `model_id` (3) | `string` | Provider-local model id (may contain `/` and `:`). |
| `display_name` (4) | `optional string` | Patch semantics: an absent field keeps the entry's current value; a present field sets it, and `""` / `0` clears it. The entry is created when missing.  Display name written as the entry's `name`; absent keeps it, `""` removes it. |
| `context_limit` (5) | `optional uint32` | Context window written as `limit.context`; absent keeps it, 0 removes it. A value outside 0..=4294967295 (or not an integer) is `invalid_argument`. |
| `output_limit` (6) | `optional uint32` | Max output tokens written as `limit.output`; absent keeps it, 0 removes it. Must not exceed `context_limit` when both are set. |
| `reasoning` (7) | `optional bool` | Reasoning switch written as `reasoning: true|false`; absent keeps the current `reasoning` field (a detailed `reasoning:` mapping is kept when this is true). Only `RemoveProviderModel` clears it. |

### `RemoveProviderModelRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory context (unused; providers are process-wide). |
| `provider_id` (2) | `string` | Configured provider id. |
| `model_id` (3) | `string` | Provider-local model id whose config entry is removed. |

### `DiscoveryOutcome`

Outcome of one remote model-list fetch.

| Field | Type | Description |
|---|---|---|
| `ok` (1) | `bool` | True when the remote model list was fetched and parsed (possibly empty). |
| `result` (2) | `string` | `models`, `empty`, `auth_required`, `auth_rejected`, `unavailable`, `invalid`, or `unsupported`. |
| `error_message` (3) | `string` | Bounded, non-secret failure description when `ok` is false. |
| `model_count` (4) | `uint32` | Number of remote models fetched (and now in the model cache). |

### `ProviderUpdate`

A provider after a change, applied live.

| Field | Type | Description |
|---|---|---|
| `provider` (1) | `ProviderInfo` | The provider with its effective model rows. |
| `discovery` (2) | `DiscoveryOutcome` | Remote model-list fetch outcome; unset when the call did not fetch. |

### `TestProviderModelRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory context (unused; providers are process-wide). |
| `provider_id` (2) | `string` | Configured provider id. |
| `model_id` (3) | `string` | Provider-local model id to probe. |

### `TestProviderModelResponse`


| Field | Type | Description |
|---|---|---|
| `ok` (1) | `bool` | True when the reply stream completed without an error (a `length` finish is a normal reply: the probe caps output at one token). |
| `text` (2) | `string` | Text the model returned (often one token or empty). |
| `finish_reason` (3) | `string` | Finish reason when the provider reported one: `stop`, `length`, `tool_calls`, `cancelled`, or `error`. |
| `error_code` (4) | `string` | Stable failure class when `ok` is false: `http_<status>`, `transport`, `timeout`, `unknown_model`, `incompatible`, `decode`, `auth_expired`, or `provider_error`. |
| `error_message` (5) | `string` | Bounded provider failure message when `ok` is false. |
| `latency_ms` (6) | `uint32` | Wall-clock time of the probe in milliseconds. |

### `ListCommandsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Optional: empty lists the global view (no project sources). |
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
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Optional: empty lists the global view (no project sources). |
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
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
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

### `PermissionModeSummary`

One selectable session permission mode.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Mode id for `UpdateSession.permission_mode` (`manual`, `yolo`, or `<bundle-id>/<mode-id>`). |
| `title` (2) | `string` | Short human-readable name. |
| `description` (3) | `string` | Longer description; empty when the bundle declares none. |
| `source` (4) | `string` | `builtin` or the id of the bundle that declares the mode. |

### `ListPermissionModesResponse`


| Field | Type | Description |
|---|---|---|
| `modes` (1) | `repeated PermissionModeSummary` | Built-in modes first, then bundle modes sorted by bundle and mode id. |

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
| `since_seq` (2) | `uint64` | Skip durable events with `seq` at or below this watermark. Live-only frames (`seq = 0`) are always delivered. No history is replayed. |
| `include_descendants` (3) | `bool` | Also deliver the live interaction frames (`permissionRequested`, `questionRequested`, `interactionResolved`) of every descendant session (subagents at any depth). Such a frame's `session` names the descendant that asked, not the streamed session. Durable events stay per session. |

### `StreamGlobalEventsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
| `since_seq` (2) | `uint64` | Skip durable events with `seq` at or below this watermark. Live-only frames (`seq = 0`) are always delivered. No history is replayed. |
| `interactions_only` (3) | `bool` | Deliver only the live interaction frames (`permissionRequested`, `questionRequested`, `interactionResolved`) of every session plus the process-wide `catalogUpdated` and `projectsUpdated` notices; skip every session's engine events and their `resync` frames. For a client that follows one session on its session stream and needs only the asks of the others. |

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
| `last_seq` (1) | `uint64` | The watermark the stream was opened with (`since_seq`); a client that tracks its own last applied durable `seq` should replay from that. |

### `StreamEvent`

One curated projected event from the event log.

| Field | Type | Description |
|---|---|---|
| `seq` (1) | `uint64` | Monotonic sequence number within the session; 0 (omitted in protojson) for live-only frames, which `ListEvents` never returns. |
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
| `part_replaced` (20) | `oneof `payload`: PartReplaced` | A text or reasoning part's whole text was set (the durable record of a streamed text part, or a plugin rewrite of it). |
| `error_reported` (21) | `oneof `payload`: ErrorReported` | A runtime error was recorded; when it names a message, the turn that drove that message failed (`MessageInfo.error`). |
| `member_updated` (22) | `oneof `payload`: MemberInfo` | A subagent spawned by this session was created or changed status (durable, on the parent session's stream). `member` is always set; the spawn frame carries every field, later frames carry `status` (and `summary`/`child` on finish) and leave the rest empty, so fold by `member`. |
| `session_reverted` (23) | `oneof `payload`: SessionReverted` | The session was reverted (durable), or its pending revert was undone (`undone`). Re-read the session (`SessionInfo.revert`) and its messages: a revert hides `messageId` and every later message; an undo brings them back. A later `messageStarted` commits a pending revert. |
| `parts_added` (24) | `oneof `payload`: PartsAdded` | Complete parts were added to a message in one step (durable): the images attached to a prompt turn, as `AttachmentPart`s without their bytes. Append them to the message after its text. |
| `catalog_updated` (25) | `oneof `payload`: CatalogUpdated` | The provider/model catalog changed (a provider was added, edited, or refreshed, a key was set or removed, or startup discovery finished). Live-only and process-wide: `seq` is 0 and `session` is empty on every stream it reaches (global and session). Re-read `ListModels` / `ListProviders`. |
| `projects_updated` (26) | `oneof `payload`: ProjectsUpdated` | The Project list changed: a Project was created, updated, or deleted, a Project gained or lost a session, or a Project's `busy` flag changed. Live-only and process-wide, delivered on the global stream only (also with `interactions_only`): `seq` is 0 and `session` is empty. Re-read `ListProjects`. |

### `PartsAdded`

The provider/model catalog changed; carries no fields.
The Project list changed; carries no fields.
Complete parts added to a message in one step.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Owning message identifier. |
| `parts` (2) | `repeated PartInfo` | The added parts, in message order. |

### `SessionReverted`

A session revert or its undo.

| Field | Type | Description |
|---|---|---|
| `message_id` (1) | `string` | The reverted user message; empty for an undo. |
| `undone` (2) | `bool` | True for an undo (`RevertSession.undo`). |
| `files` (3) | `repeated RevertedFile` | Files the operation wrote (or could not restore). |

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
| `permission_mode` (5) | `optional string` | New permission mode of the session tree when changed (emitted on the root session only). |
| `archived` (6) | `optional bool` | New archived flag of a root session when changed: `true` when it was archived, `false` when it was unarchived (explicitly or by a new turn). |

### `MessageStarted`

A session was deleted.
A message started.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Message identifier. |
| `role` (2) | `Role` | Author role. |
| `agent` (3) | `string` | Agent the assistant turn runs as; empty for user, system, and shell messages. |
| `model` (4) | `string` | Model the assistant turn requested (`provider/model`); the model that finally served it is `MessageInfo.model`. Empty where `agent` is. |

### `MessageFinished`

A message reached a terminal state.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Message identifier. |
| `finish` (2) | `FinishReason` | Terminal finish reason. |
| `usage` (3) | `TokenUsage` | Token usage of the final round when the backend accounts it here. |
| `cause` (4) | `FinishCause` | Harness cause of the finish (cancel, shutdown, crash recovery, provider failure). |

### `PartStarted`

A part was appended to a message.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Owning message identifier. |
| `part` (2) | `string` | Part identifier. |
| `kind` (3) | `string` | Part kind discriminator matching `PartInfo.kind`. |
| `tool` (4) | `string` | Tool name when `kind` is `tool_call`. |
| `call_id` (5) | `string` | Tool call id when `kind` is `tool_call`. |

### `PartAppended`

Streaming delta appended to a part. Assistant text deltas are live-only
(`seq = 0`); reasoning and legacy text deltas are durable.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Owning message identifier. |
| `part` (2) | `string` | Part identifier. |
| `text_delta` (3) | `string` | Incremental delta: text for text/reasoning parts, a raw argument JSON fragment for `tool_call` parts (append to `ToolCallPart.input_json`). |

### `PartCompleted`

A part reached its final form.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Owning message identifier. |
| `part` (2) | `string` | Part identifier. |

### `PartReplaced`

A text or reasoning part's text was replaced wholesale. Clients set the
part's text to `text` (not append); it supersedes any live deltas they
accumulated for the same part id.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Owning message identifier. |
| `part` (2) | `string` | Part identifier. |
| `text` (3) | `string` | Full text of the part. |

### `ErrorReported`

A runtime error was recorded.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Message whose turn failed; empty for errors not tied to a message. |
| `code` (2) | `string` | Stable machine code (for example `provider_error`). |
| `error_message` (3) | `string` | Human-readable error text as the engine recorded it. |

### `ToolStateChanged`

A tool call's execution state changed.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Owning message identifier. |
| `part` (2) | `string` | Part identifier of the tool call. |
| `call_id` (3) | `string` | Matching call id (empty on a direct part overwrite). |
| `state` (4) | `ToolExecutionState` | New execution state. |
| `error_code` (5) | `string` | Stable error code when the call failed. |
| `error_message` (6) | `string` | Error text when the call failed, was denied, or was blocked. |
| `input_json` (7) | `string` | Full parsed arguments as JSON text; set when the call is requested (`RUNNING`) and replaces the streamed argument fragments. |
| `output_json` (8) | `string` | Output as JSON text when the call finished OK (same cap as stored). |
| `duration_ms` (9) | `uint64` | Wall time in milliseconds when the call finished OK. |
| `tool` (10) | `string` | Tool name when known (set on `RUNNING`). |

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
| `items` (1) | `repeated TodoItem` | Full replacement todo list (the same rows `GetSessionTodo` returns). |

### `WorkflowUpdated`

The workflow projection changed.

| Field | Type | Description |
|---|---|---|
| `state` (1) | `WorkflowState` | Full replacement workflow state. |

### `TokensRecorded`

Billed usage of one provider call. Durable: one per streaming round of an
assistant message, plus side calls (title, summarizer) made for the
session, which carry no `message`.

| Field | Type | Description |
|---|---|---|
| `message` (1) | `string` | Assistant message the round belongs to; empty for a side call. |
| `usage` (2) | `TokenUsage` | Usage of this one call (not a message sum). |
| `model` (3) | `string` | Model that served the call (`provider/model`). |

### `CompactionApplied`

A compaction folded part of the context behind a summary. Emitted by the
automatic mid-turn strategies and by a manual `CompactSession`.

| Field | Type | Description |
|---|---|---|
| `until_seq` (1) | `uint64` | Sequence of this compaction record. |
| `strategy` (2) | `string` | Strategy that fired: `native`, `local_summarizer`, `snap_compact`, or `handoff` (a manual compaction reports `local_summarizer`). |
| `message` (3) | `string` | System message carrying the summary (the transcript divider). |
| `folded_count` (4) | `uint32` | Number of messages folded behind the summary. |
| `manual` (5) | `bool` | Whether a client asked for it (`CompactSession`) rather than the context crossing its threshold. |

### `ReadFileRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Required: without one the rpc fails with invalid_argument (the backend has no working directory of its own). |
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
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Required: without one the rpc fails with invalid_argument (the backend has no working directory of its own). |
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
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Required: without one the rpc fails with invalid_argument (the backend has no working directory of its own). |
| `pattern` (2) | `string` | Glob pattern matched against relative paths (`**/*.rs`). |
| `limit` (3) | `uint32` | Maximum paths to return; 0 uses the server default. |

### `FindFilesResponse`


| Field | Type | Description |
|---|---|---|
| `paths` (1) | `repeated string` | Matching relative paths. |

### `SearchTextRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Required: without one the rpc fails with invalid_argument (the backend has no working directory of its own). |
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
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Required: without one the rpc fails with invalid_argument (the backend has no working directory of its own). |
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
| `payload` (7) | `google.protobuf.Struct` | Structured payload as a JSON object. For a permission request: `action` (permission action, e.g. `bash`, `edit`), `resource` (the pattern being decided, e.g. the command or path), `always` (patterns an "always" reply saves), and, when the ask is correlated with a tool call, `messageId`, `callId`, `tool` (tool name), and `input` (the call's parsed arguments object exactly as recorded on the tool part: e.g. `command` for bash; the path and old/new text or patch for edit tools). |
| `time_created` (8) | `google.protobuf.Timestamp` | When the request was created. |

### `ListInteractionsRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
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

A persisted permission decision: an "allow always" reply. Most saved rules
are global (shared by every session and project) and reload into the
permission plane when the server starts; an `ExternalDirectory` grant
(ADR-0026) is scoped to one Project instead. Deleting a rule revokes it
live.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Rule identifier. |
| `permission` (2) | `RulePermission` | Effect of the rule; always RULE_PERMISSION_ALLOW for saved grants. |
| `tool` (3) | `string` | Tool the rule matches: the exact tool or MCP tool name for a tool grant, `bash` for a command grant, or the action name (`read`, `edit`, `webfetch`, ...) for an action-wide grant. |
| `pattern` (4) | `string` | Pattern the rule matches: the exact command for a `bash` grant, `*` for an action-wide grant, empty for an exact tool grant. |
| `time_created` (5) | `google.protobuf.Timestamp` | When the rule was saved; absent for rules saved before creation times were recorded. |
| `project_id` (6) | `string` | Project the rule is scoped to, or the literal `"global"` for a rule that applies to every session and project (ADR-0026's `GLOBAL_PROJECT`; every pre-ADR-0026 row is global). |

### `ListSavedRulesRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Accepted for symmetry and ignored: this lists rules of every Project (and global rules) regardless of directory; see `SavedRule.project_id`. |
| `page` (2) | `PageRequest` | Standard pagination controls. |

### `ListSavedRulesResponse`


| Field | Type | Description |
|---|---|---|
| `rules` (1) | `repeated SavedRule` | Saved rules in stable id order. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `DeleteSavedRuleRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Accepted for symmetry and ignored: a rule id is unique across every Project (and global rules); see `SavedRule.project_id`. |
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
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |

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
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
| `name` (2) | `string` | Server name used in tool namespaces. |
| `command` (3) | `oneof `transport`: CommandTransport` | Transport definition; exactly one kind is set. Launch a local stdio server. |
| `url` (4) | `oneof `transport`: UrlTransport` | Connect to a remote HTTP server. |
| `enabled` (5) | `optional bool` | Whether the server starts enabled; defaults to true. |

### `ConnectMcpRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
| `name` (2) | `string` | Server name to connect. |

### `DisconnectMcpRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
| `name` (2) | `string` | Server name to disconnect. |

### `StartMcpAuthRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
| `name` (2) | `string` | Server name to authenticate. |

### `StartMcpAuthResponse`


| Field | Type | Description |
|---|---|---|
| `authorization_url` (1) | `string` | Authorization URL the client must open in a browser. |

### `CompleteMcpAuthRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
| `name` (2) | `string` | Server name completing the flow. |
| `code` (3) | `string` | Authorization code returned by the provider callback. |

### `RemoveMcpAuthRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
| `name` (2) | `string` | Server name whose credentials should be removed. |

### `TokenUsage`

Token accounting for one model round.

| Field | Type | Description |
|---|---|---|
| `input` (1) | `uint64` | Uncached input tokens of the round; excludes `cache_read` and `cache_write`, so the whole prompt is their sum. |
| `output` (2) | `uint64` | All generated tokens of the round, thinking included. |
| `reasoning` (3) | `uint64` | Thinking tokens within `output` when the provider reports them; 0 when it does not (the thinking split is then unknown, never estimated). |
| `cache_read` (4) | `uint64` | Prompt tokens served from cache. |
| `cache_write` (5) | `uint64` | Prompt tokens written to cache (cache creation). |
| `reasoning_unknown` (6) | `bool` | The provider did not report the thinking share of `output` (for a sum: at least one summed call did not), so `reasoning` undercounts. |

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

A tool invocation requested by the model. One part covers the whole call:
arguments, execution state, and (once finished) its output or error.

| Field | Type | Description |
|---|---|---|
| `call_id` (1) | `string` | Engine-issued call id. It is `MemberInfo.call_id` of a subagent the call spawned. |
| `tool` (2) | `string` | Canonical tool name. |
| `input_json` (3) | `string` | Tool input as JSON text, in every state (empty while the arguments are still streaming and unparsed). |
| `state` (4) | `ToolExecutionState` | Execution state of the call. |
| `error_code` (5) | `string` | Stable structured error type when the call failed (e.g. `unknown`). |
| `error_message` (6) | `string` | Error text when the call failed, denied, or was blocked. |
| `output_json` (7) | `string` | Tool output as JSON text when the call finished OK: the stored output, under the same size cap the model saw. For the `task` tool it is `{title, metadata: {sessionId, parentSessionId, subagent_type, status}, output}`; `metadata.sessionId` is the child session. |
| `duration_ms` (8) | `uint64` | Wall time of a finished OK call in milliseconds. |

### `ToolResultPart`

The outcome of a tool invocation.

| Field | Type | Description |
|---|---|---|
| `call_id` (1) | `string` | Matching call id from `ToolCallPart.call_id`. |
| `output` (2) | `string` | Tool output payload (text or serialized JSON). |
| `error_code` (3) | `string` | Stable error code when the call failed. |
| `error_message` (4) | `string` | Human-readable error text when the call failed. |

### `AttachmentPart`

A binary or file attachment on a message (for example an image attached
to a prompt turn).

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Attachment file name. |
| `mime` (2) | `string` | MIME type when known. |
| `data` (3) | `bytes` | Inline payload. Transcript reads (`ListMessages`, `GetMessage`, the `partsAdded` stream frame) leave it empty: prompt images are stored once in the session's blob store and only sent to the model. |
| `path` (4) | `string` | The client-side path the attachment was read from, when it sent one. |
| `size` (5) | `uint64` | Size of the attachment in bytes. |

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
| `agent` (4) | `string` | Agent the message's turn ran as (assistant messages); empty for user, system, and shell messages and for messages recorded before 0.41.0. |
| `model` (5) | `string` | Model that produced the message (`provider/model`): the model that served its latest round (after chat.params, fallback, or routing), else the model its turn requested. Empty when unknown. |
| `finish` (6) | `FinishReason` | Terminal finish reason for assistant messages. |
| `parts` (7) | `repeated PartInfo` | Ordered message parts. |
| `time_created` (8) | `google.protobuf.Timestamp` | When the message was created (its `MessageStarted` event). |
| `time_updated` (9) | `google.protobuf.Timestamp` | When the message projection last changed (parts, live deltas, usage, error, finish). |
| `finish_cause` (10) | `FinishCause` | Harness cause of the finish (cancel, shutdown, crash recovery, provider failure). |
| `error` (11) | `MessageError` | Why the turn that drove this assistant message failed; set only when the engine recorded an error for it (`finish` is then `FINISH_REASON_ERROR`). |
| `usage` (12) | `TokenUsage` | Billed usage of the assistant message: the sum of its provider rounds (`TokensRecorded`), or the finish total for messages recorded before per-round usage. Unset when no usage was reported. |
| `round_usage` (13) | `TokenUsage` | Usage of the message's latest provider round (the request `model` served last). Its `input + cache_read + cache_write` is the prompt that round sent: the session's context occupancy against the model's `ModelSummary.context_limit`. Unset without per-round usage. |

### `MessageError`

Recorded failure of the turn behind an assistant message.

| Field | Type | Description |
|---|---|---|
| `code` (1) | `string` | Stable machine code (for example `provider_error`). |
| `message` (2) | `string` | Human-readable error text as the engine recorded it. |

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

### `MemberInfo`

A subagent (member) spawned by a session, as recorded on the parent's log.

| Field | Type | Description |
|---|---|---|
| `member` (1) | `string` | Member id within the parent session. |
| `child` (2) | `string` | Child session id when known. |
| `agent` (3) | `string` | Subagent type (agent name) of the spawn. |
| `description` (4) | `string` | Short description of the delegated task. |
| `status` (5) | `MemberStatus` | Latest lifecycle status. |
| `summary` (6) | `string` | Bounded finish summary; empty until the member finished. |
| `call_id` (7) | `string` | Tool call (`ToolCallPart.call_id`) that spawned the member; empty for members started without a tool call. |
| `depth` (8) | `uint32` | Depth in the subagent tree (children of a root session are 1). |

### `RevertedFile`

One file a session revert or unrevert wrote (or could not restore).

| Field | Type | Description |
|---|---|---|
| `path` (1) | `string` | Absolute path of the file. |
| `action` (2) | `string` | What happened: `restored` (content written back), `deleted` (the file did not exist at that point, so it was removed), `unchanged` (already in that state), `skipped` (its content was not kept — see `reason`), or `failed` (writing it failed — see `reason`). |
| `reason` (3) | `string` | Why a file was `skipped` (`too_large`, `session_cap`, `snapshot_budget`, `unreadable`) or the error of a `failed` write; empty otherwise. |

### `GetHealthResponse`


| Field | Type | Description |
|---|---|---|
| `ok` (1) | `bool` | Always `true` when the endpoint answers successfully. |
| `version` (2) | `string` | Backend version string (workspace release version). |

### `LocationInfo`

Where this backend runs and which directory it serves.

| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | The request's directory scope, empty when it named none: the backend has no working directory of its own. |
| `hostname` (2) | `string` | Hostname of the machine running the backend. |
| `pid` (3) | `uint32` | OS process id of the backend. |
| `version` (4) | `string` | Backend version string (workspace release version). |

### `GetConfigRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |

### `GetConfigResponse`


| Field | Type | Description |
|---|---|---|
| `values` (1) | `google.protobuf.Struct` | Effective merged configuration as a JSON object. |

### `UpdateConfigRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Ignored: this rpc does not depend on a directory. |
| `patch` (2) | `google.protobuf.Struct` | JSON object deep-merged into the stored config. |

### `UpgradeProcessResponse`


| Field | Type | Description |
|---|---|---|
| `status` (1) | `string` | Human-readable outcome of the upgrade attempt. |

### `GetBootstrapRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Optional: empty lists the global view (no project sources). |

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

One Project.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Project identifier (`prj_...`). |
| `name` (3) | `string` | Display name. |
| `roots` (4) | `repeated string` | Absolute root directories on the backend machine, primary root first. Never empty. |
| `created_at` (5) | `google.protobuf.Timestamp` | When the Project was created. |
| `updated_at` (6) | `google.protobuf.Timestamp` | When the Project was last renamed or its roots replaced. |
| `session_count` (7) | `uint32` | Root sessions (not subagent sessions) of the Project that still exist, archived ones included. |
| `busy` (8) | `bool` | Whether a non-archived session of the Project is running a turn now (the same run state as `SessionInfo.busy`). Live changes arrive as `projectsUpdated` frames on the global event stream. |

### `ListProjectsRequest`


| Field | Type | Description |
|---|---|---|
| `page` (1) | `PageRequest` | Standard pagination controls. |

### `ListProjectsResponse`


| Field | Type | Description |
|---|---|---|
| `projects` (1) | `repeated ProjectInfo` | Projects, most recently updated first. |
| `page` (2) | `PageInfo` | Pagination outcome. |

### `GetCurrentProjectRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope; the `x-hya-directory` header overrides it. |

### `ResolveProjectRequest`


| Field | Type | Description |
|---|---|---|
| `path` (1) | `string` | Absolute path to match against every Project's roots. |

### `ResolveProjectResponse`


| Field | Type | Description |
|---|---|---|
| `project` (1) | `ProjectInfo` | The Project whose root contains `path`; unset when none does. |

### `EnsureProjectForPathRequest`


| Field | Type | Description |
|---|---|---|
| `path` (1) | `string` | Absolute working directory. |

### `EnsureProjectForPathResponse`


| Field | Type | Description |
|---|---|---|
| `project` (1) | `ProjectInfo` | The matching or newly created Project. |
| `created` (2) | `bool` | Whether this call created the Project. |

### `CreateProjectRequest`


| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | Display name (non-empty after trimming). |
| `roots` (2) | `repeated string` | Absolute root directories, primary root first (at least one). |

### `GetProjectRequest`


| Field | Type | Description |
|---|---|---|
| `project` (1) | `string` | Project identifier. |

### `UpdateProjectRequest`


| Field | Type | Description |
|---|---|---|
| `project` (1) | `string` | Project identifier. |
| `name` (2) | `optional string` | New display name when set. |
| `roots` (3) | `repeated string` | New root list, primary root first, replacing the whole list; empty keeps the current roots. |

### `DeleteProjectRequest`


| Field | Type | Description |
|---|---|---|
| `project` (1) | `string` | Project identifier. |

### `ListProjectDirectoriesRequest`


| Field | Type | Description |
|---|---|---|
| `project` (1) | `string` | Project identifier. |

### `ListProjectDirectoriesResponse`


| Field | Type | Description |
|---|---|---|
| `directories` (1) | `repeated string` | The Project's roots, primary root first. |

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
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Required: without one the rpc fails with invalid_argument (the backend has no working directory of its own). |

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
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Required: without one the rpc fails with invalid_argument (the backend has no working directory of its own). |
| `raw` (2) | `bool` | Accepted and ignored: the diff is always git's unified patch (`git diff HEAD` plus untracked files). |
| `paths` (3) | `repeated string` | Restrict to these paths (git pathspecs relative to the scope directory: a file or a directory prefix); empty diffs everything. Over HTTP repeat the query key (`?paths=a&paths=b`). A path that is absolute or contains `..` is `invalid_argument`. |

### `GetVcsDiffResponse`


| Field | Type | Description |
|---|---|---|
| `diff` (1) | `string` | Diff payload in the requested format. |

### `ApplyPatchRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Required: without one the rpc fails with invalid_argument (the backend has no working directory of its own). |
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
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Required when `cwd` is empty: without either the rpc fails with invalid_argument. |
| `shell` (2) | `string` | Shell binary name or path; empty picks the default shell. |
| `cols` (3) | `uint32` | Initial terminal width in columns. |
| `rows` (4) | `uint32` | Initial terminal height in rows. |
| `cwd` (5) | `string` | Absolute working directory for the shell; defaults to the scope directory. |
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
| `permission_mode` (12) | `string` | Effective permission mode of the session tree: `manual`, `yolo`, or `<bundle-id>/<mode-id>`. Recorded on the root session (children report the root's mode); the process default (`yolo` under `--yolo` or `permission.model: danger`, else `manual`) when none was set. |
| `members` (13) | `repeated MemberInfo` | Subagents this session spawned, in spawn order, with their latest status (folded from the session's own log). The live counterpart is the `memberUpdated` stream event. |
| `usage` (14) | `TokenUsage` | Billed usage of the session: every provider call made for it (turn rounds plus title/summarizer side calls), summed. Never decreases (compaction, revert, and deletion keep billed usage). Unset when none. |
| `forked_from` (15) | `ForkSource` | Source of a forked session; unset for sessions that are not forks. |
| `revert` (16) | `SessionRevert` | Pending revert (`RevertSession`): its messages are hidden from `ListMessages` until an undo restores them or the next prompt or shell turn commits the revert. Unset when nothing is pending. |
| `archived` (17) | `bool` | Whether this root session is archived: hidden from `ListSessions` unless requested. Subagent child sessions are never archived; they follow their root. Archiving does not cancel a running turn. |
| `archived_at` (18) | `google.protobuf.Timestamp` | When the session was archived; unset when it is not archived. |
| `project_id` (19) | `string` | Project the session belongs to (a subagent session carries its root's); empty for a temporary session or one created before Projects existed. The id may name a Project that was deleted since. |
| `kind` (20) | `SessionKind` | Kind of the session: `SESSION_KIND_PROJECT` or `SESSION_KIND_TEMPORARY`. |

### `ForkSource`

Where a forked session came from.

| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Source session id. |
| `message_id` (2) | `string` | Source user message the fork was cut before (the fork holds the messages strictly before it); empty for a head fork. |

### `SessionRevert`

A pending revert of a session.

| Field | Type | Description |
|---|---|---|
| `message_id` (1) | `string` | The reverted user message (the first hidden message). |
| `text` (2) | `string` | Text of that user message, e.g. to put it back in the composer. |
| `hidden_messages` (3) | `uint32` | Number of hidden messages (the reverted message and every later one). |
| `files` (4) | `repeated RevertedFile` | Files the revert restored, each as the revert left it. |

### `CreateSessionRequest`

Where a new root session works is chosen by the client (ADR-0024):

- `kind = SESSION_KIND_TEMPORARY`: no Project; the server creates the
session's scratch directory and uses it as the workdir. `project_id` and
`workdir` must be unset (`invalid_argument`).
- `project_id` set (kind `SESSION_KIND_PROJECT` or unset): the Project must
exist (`not_found`). `workdir`, when set, must lie inside one of its roots
(`invalid_argument` otherwise); unset means the primary root.
- no `project_id`, `workdir` set (kind `SESSION_KIND_PROJECT` or unset): the
Project is found or created as by `EnsureProjectForPath(workdir)` and the
session works in `workdir` (a local client passes its cwd).
- neither: `invalid_argument`.

A child session (`parent` set) always joins its parent's Project and kind:
`project_id` and `kind` must be unset (`invalid_argument`); `workdir`
defaults to the parent's. `workdir` must be absolute without `..`
components.

| Field | Type | Description |
|---|---|---|
| `agent` (1) | `string` | Agent name or catalog id to bind as the session's default agent. |
| `model` (2) | `string` | Model reference the session starts on (`provider/model[#variant]`). |
| `workdir` (3) | `optional string` | Absolute workdir for tools and relative paths in this session; see the rules above. |
| `parent` (4) | `string` | When set, marks the new session as a child of this parent id. |
| `initialize` (5) | `bool` | When true, run the directory initialization turn after creation. |
| `title` (6) | `string` | Initial title; empty lets the backend derive one. |
| `project_id` (7) | `string` | Project of the new root session. |
| `kind` (8) | `SessionKind` | Kind of the new root session; unset means `SESSION_KIND_PROJECT`. |

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
| `parent` (2) | `string` | Restrict to direct children of this session id when non-empty. |
| `page` (3) | `PageRequest` | Standard pagination controls. |
| `include_archived` (4) | `bool` | Also list archived root sessions (default: they are left out). |
| `archived_only` (5) | `bool` | List only archived root sessions (implies `include_archived`). |
| `project_id` (6) | `string` | Restrict to the sessions (root and subagent) of this Project when non-empty; an id that names no Project lists nothing. |

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
| `permission_mode` (6) | `optional string` | New permission mode for the whole session tree when set: `manual`, `yolo`, or a `<bundle-id>/<mode-id>` listed by `ListPermissionModes`. Unknown or unavailable modes are rejected with `invalid_argument`. Switching to `yolo` also allows (once) every pending permission ask of the tree. |
| `archived` (7) | `optional bool` | Archive (`true`) or unarchive (`false`) a root session when set. Idempotent: archiving an archived session keeps its first `archived_at`. Archiving a subagent child session is `invalid_argument`; unarchiving one is a no-op. Archiving does not cancel a running turn. A new prompt, command, or shell turn unarchives the session implicitly. |

### `DeleteSessionRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier. |

### `ForkSessionRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier to fork from. |
| `until_seq` (2) | `uint64` | Copy the messages whose start was recorded at or before this sequence number; 0 forks at the current head. Ignored when `message_id` is set. |
| `message_id` (3) | `string` | Fork before this user message of the source: the new session holds every message strictly before it. Empty forks at `until_seq` (or the head). A message that is not a visible user message of the source is `invalid_argument` (not a user message) or `not_found`. |

### `ForkSessionResponse`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `SessionInfo` | Projection summary of the forked session. |
| `prompt_text` (2) | `string` | Text of the user message the fork was cut before (`message_id`), e.g. to prefill the composer; empty for a head or `until_seq` fork. |

### `CompactSessionRequest`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `string` | Session identifier to compact. |
| `until_seq` (2) | `uint64` | Deprecated and ignored: a manual compaction always folds the whole transcript at the head. The response's `compacted_until_seq` reports the watermark actually reached. |

### `CompactSessionResponse`


| Field | Type | Description |
|---|---|---|
| `compacted_until_seq` (1) | `uint64` | Watermark the context was compacted up to. |
| `strategy` (2) | `string` | Compaction strategy that ran, the same name the recorded `CompactionApplied.strategy` carries. A manual compaction is always `local_summarizer`. |

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
| `until_seq` (2) | `uint64` | Deprecated: sequence targets are not supported; a nonzero value is `invalid_argument`. Use `message_id`. |
| `undo` (3) | `bool` | When true, undo the pending revert (`/redo`): the hidden messages come back and the files are written back to their state before the revert. `invalid_argument` when no revert is pending (none, or committed by a later prompt). |
| `message_id` (4) | `string` | User message to revert to (it and every later message are hidden). Empty reverts the last visible user message (`/undo`); repeating it reverts further back. `not_found` when the message is not in the session, `invalid_argument` when it is not a user message or already reverted. |

### `RevertSessionResponse`


| Field | Type | Description |
|---|---|---|
| `session` (1) | `SessionInfo` | Projection summary after the revert (`revert` set after a revert, unset after an undo). |
| `files` (2) | `repeated RevertedFile` | Files this call wrote (or could not restore). |

### `PromptTurn`

A plain user prompt turn.

| Field | Type | Description |
|---|---|---|
| `text` (1) | `string` | User text recorded as the next user message. |
| `attachments` (2) | `repeated PromptAttachment` | Images sent to the model with the text (at most 10 MiB each and 20 MiB per turn). Recorded atomically with the user message; listed back as `AttachmentPart`s without their bytes. A bad attachment, or one for a model that declares no image input, fails the call with `invalid_argument` and admits nothing. |

### `PromptAttachment`

One image attached to a prompt turn.

| Field | Type | Description |
|---|---|---|
| `name` (1) | `string` | File name shown in the transcript and sent to the model (required). |
| `mime` (2) | `string` | MIME type: `image/png`, `image/jpeg`, `image/gif`, or `image/webp`. Empty lets the server detect it from the bytes; when set it must match them. |
| `data` (3) | `bytes` | The image bytes (standard base64 in protojson). |
| `path` (4) | `string` | Where the client read the file from; recorded for display only, never read by the server. |

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
no model round. The user's own shell command never asks for permission in
any mode (approved once); an explicit Deny rule still blocks it, a
`tool.execute.before` hook can still veto it, and a directory outside the
working directory still asks.

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

Projection snapshot of one admitted turn. For prompt and command turns
the turn id is the id of the admitted **user** message; the assistant
message(s) the engine drives for it arrive on the event stream
(`messageStarted` with `ROLE_ASSISTANT`). For shell turns it is the id of
the synthetic assistant message. Slash commands intercepted by workflow
features return an empty id.

| Field | Type | Description |
|---|---|---|
| `id` (1) | `string` | Turn identifier (user message id for prompt/command turns). |
| `session` (2) | `string` | Owning session identifier. |
| `state` (3) | `TurnState` | Current lifecycle state. |
| `finish` (4) | `FinishReason` | Terminal finish reason once state is FINISHED. |
| `error_code` (5) | `string` | Stable error code when state is FAILED and the engine recorded an error for the failed assistant message (for example `provider_error`). |
| `error_message` (6) | `string` | Human-readable error message when state is FAILED (same source as `MessageInfo.error`). |

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
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Required: without one the rpc fails with invalid_argument (the backend has no working directory of its own). |

### `ListWorktreesResponse`


| Field | Type | Description |
|---|---|---|
| `worktrees` (1) | `repeated Worktree` | Worktrees of the repository. |

### `CreateWorktreeRequest`


| Field | Type | Description |
|---|---|---|
| `directory` (1) | `string` | Directory scope (absolute; or the x-hya-directory header). Required: without one the rpc fails with invalid_argument (the backend has no working directory of its own). |
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

### `AgentModelSource`

Which tier resolved an agent's effective base model.

| Value | Number | Description |
|---|---|---|
| `AGENT_MODEL_SOURCE_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `AGENT_MODEL_SOURCE_SESSION` | 1 | An explicit override captured for the current root session tree. |
| `AGENT_MODEL_SOURCE_CONFIGURED` | 2 | The agent has an explicit direct model or category policy. |
| `AGENT_MODEL_SOURCE_REMEMBERED` | 3 | A durable preference retained and matching the current catalog. |
| `AGENT_MODEL_SOURCE_DEFAULT` | 4 | No configured or retained model; the process base is used. |

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

### `FinishCause`

Why the harness (not the model) ended an assistant message. Context on
top of FinishReason; unset when the model ended the message itself.

| Value | Number | Description |
|---|---|---|
| `FINISH_CAUSE_UNSPECIFIED` | 0 | No harness cause recorded. |
| `FINISH_CAUSE_USER_CANCEL` | 1 | A user stopped the turn (client abort, SIGINT on a one-shot run). |
| `FINISH_CAUSE_SHUTDOWN` | 2 | The process stopped gracefully and drained in-flight turns. |
| `FINISH_CAUSE_LEADER_FAILED` | 3 | The member was stopped because its team lead's turn failed. |
| `FINISH_CAUSE_INTERRUPTED` | 4 | The process died with the turn open; closed by startup crash recovery. |
| `FINISH_CAUSE_PROVIDER_ERROR` | 5 | The model provider failed the turn. |
| `FINISH_CAUSE_OTHER` | 6 | A cause this server build does not name. |
| `FINISH_CAUSE_ARCHIVED` | 7 | The member's parent archived it (`archive` tool) while it was mid-turn. |

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
| `TODO_STATUS_BLOCKED` | 4 | Waiting on an external unblock (dependency, user input, review). |

### `MemberStatus`

Lifecycle status of a spawned subagent (member).

| Value | Number | Description |
|---|---|---|
| `MEMBER_STATUS_UNSPECIFIED` | 0 | Unset sentinel; never emitted by the server. |
| `MEMBER_STATUS_SPAWNING` | 1 | The child session is being created or admitted. |
| `MEMBER_STATUS_RUNNING` | 2 | The child is running a turn. |
| `MEMBER_STATUS_DONE` | 3 | The child finished successfully. |
| `MEMBER_STATUS_FAILED` | 4 | The child failed. |
| `MEMBER_STATUS_CANCELLED` | 5 | The child was cancelled. |

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

### `SessionKind`

Kind of a session (ADR-0024).

| Value | Number | Description |
|---|---|---|
| `SESSION_KIND_UNSPECIFIED` | 0 | Unset. On `CreateSessionRequest` it means `SESSION_KIND_PROJECT`; the server never reports it. |
| `SESSION_KIND_PROJECT` | 1 | A session of a Project: its workdir lies inside the Project's roots (sessions created before Projects existed have no Project). |
| `SESSION_KIND_TEMPORARY` | 2 | A session of no Project, working in its own fresh scratch directory `$XDG_CACHE_HOME/hya/scratch/<session id>` (fallback `$HOME/.cache/hya/scratch/<session id>`), which hya never deletes. |

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

