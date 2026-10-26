# Use a ChatGPT plan with Hya

Hya supports OpenAI's official **Sign in with ChatGPT** flow for eligible
ChatGPT plan usage. You authorize Hya in the browser, then select a model made
available to that account. Hya continues to run its own agent loop, tools,
permissions, transcripts and subagents. Requests use the public Responses API.
A Platform API key remains a separate, independently billed connection; this
feature uses the explicitly granted ChatGPT plan permission.

Availability and limits are controlled by OpenAI and the selected account or
workspace. Identity sign-in alone is insufficient: the grant must include
`chatgpt.tokens.use.direct` and `resource.invoke`. A successful model test or
turn verifies access; a model catalog entry alone does not. Manage app limits
and access in [ChatGPT Settings → Usage](https://chatgpt.com/settings/usage).
See [OpenAI's sign-in contract](https://developers.openai.com/siwc/token-sharing-open-source/sign-in),
[models and inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference)
and [preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations).

## Connect from the TUI

1. Run **`/key login chatgpt`** for **Continue with ChatGPT**. For an existing
   named ChatGPT connection use `/key login <provider-id>`. The common command
   chooses authentication from that provider's configured protocol; it never
   overwrites an existing URL or protocol. In `/key`, `k` logs in to the selected
   provider through the same flow.
2. Press **F2** in the login window to copy the full authorization URL and
   paste it into your browser. Where OpenTUI recognizes hyperlink support,
   you can also Ctrl+click the URL; its link target survives display wrapping.
   Choose the account or workspace and allow ChatGPT plan usage.
   The callback listener already runs
   on the backend's `127.0.0.1` with a fresh available port.
3. Return to Hya and press Enter with the callback field empty. Hya waits for
   the local callback, verifies the signed identity and granted scopes, stores
   credentials privately, and rebuilds the provider and catalog live.
4. Hya opens the provider's models and, when the session was using the offline
   model, offers the model picker. Choose a model there or with `/model`, then
   send a message.
   The model row's `t` action performs a real access test; it consumes plan
   usage. The preview route does not accept an output-token cap, even though
   other providers' model tests request one.

For example, `/key login chatgpt` → browser consent → Enter connects provider `chatgpt`.
For Windows Terminal over SSH, F2 sends the URL to the terminal on your local
computer; it does not use a clipboard program on the remote host. The login
window shows a copy notice. If your terminal disables OSC 52 clipboard writes,
allow them in the terminal. The TUI can detect a local
send failure, but cannot confirm that the terminal accepted the clipboard write.

The current OpenTUI 0.5.12 native renderer does not recognize Windows Terminal
XTVERSION as hyperlink-capable over SSH, so an explicit link alone does not
guarantee Ctrl+click there. F2 is the supported Windows Terminal path while
that upstream [terminal detection limitation](https://github.com/anomalyco/opentui/blob/v0.5.12/packages/native/src/terminal.zig) remains. This does not affect
OAuth completion or the copied URL.

Run `/model`, select a `chatgpt/<account-model-slug>` row returned by the server,
and say `hello`. No guessed model is saved when discovery fails; reconnect or
refresh with `r` after resolving the error.

For a remote backend, the browser's loopback is your local computer. Either
forward the callback port printed inside `redirect_uri` through SSH, or let the
browser reach its unreachable callback and **paste the full address-bar URL**
into Hya's Callback URL field, then press Enter. It includes `code`, `state` and,
for a new registration, `client_id`. The field is concealed; keep the URL out of
chat prompts and logs. Use the URL from the current attempt, with its exact port.
Esc cancels the frontend form; an unused pending login expires after ten minutes.

## CLI and separate account registrations

```sh
hya oauth login --provider chatgpt --type openai-chatgpt
hya oauth status chatgpt
# Give another account/workspace its own provider label:
hya oauth login --provider work-chatgpt --type openai-chatgpt --no-browser
# End its renewable session and clear local tokens:
hya oauth logout work-chatgpt
```

CLI login opens a system browser unless `--no-browser` is passed and waits for
the loopback callback. When running it over SSH, forward the selected callback
port before completing consent, or use the TUI's manual callback field. Device
login is not part of this flow. Each provider label retains its own issued
registration and validated identity; reconnecting must match that identity.
Use a new provider label for a different account. In the TUI, add another named
provider with protocol `openai-chatgpt`, the public base below and no API key,
then select it and press `k` (or run `/key login <provider-id>`). Selecting its model selects that registration.

Removing the saved credential with `x` in `/key` attempts remote revocation and
then clears the local tokens. A warning says when remote revocation was not
confirmed, with a link to ChatGPT settings. The issued registration and host ID
are retained for later login. Use `hya oauth logout <provider>` for the same
operation outside the TUI.

## Configuration and interfaces

Successful login writes a non-secret declaration; discovered models are runtime
catalog data, not a hard-coded list in the configuration:

```yaml
providers:
  chatgpt:
    kind: openai-chatgpt
    base_url: https://api.openai.com/v1
    models: []
```

`kind: openai-chatgpt` requires exactly `https://api.openai.com/v1`. It sends
`Authorization: Bearer <access_token>` to `/models` and `/responses`. The catalog
uses visible (`visibility: "list"`) entries from `models[]`, preserving `slug`,
`display_name`, and source order. Responses use `store:false`, `stream:true`, a
complete `input` history, developer messages for system history and a `hya` tool
namespace. Temperature, output-token caps, hosted tools and server-side
conversation continuation are not sent. Encrypted reasoning is replayed through
the existing Responses decoder. Explicit failed/incomplete streams keep their
existing error/finish semantics; a broken stream is not a completed answer.

The existing `hya.v1.Auth` endpoints now implement this flow over HTTP and gRPC:

| Operation | Request | Response |
| --- | --- | --- |
| `StartOauth` / `POST /v1/auth/{provider_id}/oauth/start` | `{directory?:string, providerId?:string}`; path selects the provider | `{authorizationUrl:string, state:string}`. Creates a ten-minute, one-use pending attempt. |
| `CompleteOauth` / `POST /v1/auth/{provider_id}/oauth/callback` | `{directory?:string, providerId?:string, state:string, code?:string}`; `code` is empty to wait for loopback, or a full callback URL for manual completion | `{status:"AUTH_STATUS_CREDENTIALED"}` after verification, local save, route rebuild and account model discovery attempt. Catalog failure leaves login saved but does not fabricate models. |
| `RemoveProviderAuth` / `DELETE /v1/auth/{provider_id}` | No body | `{provider?:ProviderInfo, warning?:string}`; warning is nonempty if local sign-out finished without confirmed remote revocation. |

Pending state is backend-local and scoped to the provider. Invalid, expired,
replayed or mismatched state is rejected. The callback must match the pending
loopback origin and `/auth/callback` path, without duplicate query parameters.
Only the issued client ID is used for exchange/refresh; `dynamic_agent_client`
is the initial registration entrypoint. A provider configuration changed during
login cannot receive the new credentials.

Credentials remain under `$XDG_CONFIG_HOME/hya/auth` (otherwise
`~/.config/hya/auth`), atomically written with mode `0600` on Unix:

- `<provider>.yaml`: existing OAuth fields `type: oauth`,
  `oauth_type: openai-chatgpt`, `access_token:string`, `refresh_token:string`,
  `expires_at:string` (RFC3339), `id_token?:string`, plus
  `chatgpt:{client_id:string,host_id:string,subject:string,email?:string,scopes:string[]}`.
- `.chatgpt-host.json`: installation host ID string, `urn:uuid:<UUID-v4>`.
- `.<provider>.chatgpt.json`: `{client_id:string,identity?:ChatgptRegistration}`;
  retained before exchange so a failed code exchange can reuse the registration.
- `.<provider>.chatgpt-refresh.lock`: cross-process refresh coordination on
  supported Unix hosts. Updated rotating tokens are saved together under the
  lock; access-token resolution also retains the existing process single flight.

ID-token verification checks OpenAI's JWKS signature, issuer, issued-client
audience, expiry, required identity claims and the attempt's nonce. Tokens are
never returned by these OAuth endpoints or copied from another application's
credential store. The frontend requires backend **0.45.11** for this feature.

## Login window terminal interface

The authorization text carries OpenTUI hyperlink metadata for OSC 8, targeting the exact
`StartOauth.authorizationUrl: string` returned by the backend. When emitted by the renderer, opening it is
handled by the client terminal, without launching a browser on the backend host.
The URL remains visible as text in terminals without hyperlink support.

While the ChatGPT login form and a pending OAuth URL are present, unmodified
**F2** invokes the contextual `ProviderCommand` `{kind: "copyLoginUrl"}`. It
sends that same full URL, without display wrapping or truncation, through the
existing terminal `copyText(text: string): boolean` transport (OSC 52 clipboard
selection `c`, UTF-8 encoded as base64). A `true` transport result displays
`Login URL copied to clipboard`; `false` displays the existing OSC 52 copy
failure message. This is a send result, not a terminal clipboard acknowledgement.
The action makes no RPC, changes no provider configuration, and leaves the
concealed callback field intact. F2 outside this form has no provider copy
action; Ctrl/Alt/Shift+F2 do not invoke it. Existing Enter/Esc behavior and
HTTP/gRPC OAuth contracts are unchanged.
