# SDK & Client Integration (`hya-sdk`)

You can integrate with a running Hya v2 backend from any language over `/v2` HTTP/JSON+SSE or gRPC, or use Hya's official typed Rust client crate **`hya-sdk`** (`crates/hya-sdk`).

---

## 1. Rust SDK (`crates/hya-sdk`)

`hya-sdk` wraps `/v2` HTTP + SSE into strongly typed async Rust methods (`Sdk`, `Client`) and includes **`SessionMirror`**, an in-memory reducer that folds live `StreamFrame` events—including `ExtensionEvent` frames and `SessionInfo.extensions` state—into a ready-to-render transcript view.

### Connecting & running a turn

```rust,ignore
use futures::StreamExt;
use hya_sdk::{Sdk, SessionMirror};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to a local daemon (bypasses env HTTP proxies, 5s connect timeout)
    let sdk = Sdk::for_local_daemon("http://127.0.0.1:8080", "/home/me/my-app")?;

    // 1. Fetch startup catalog (models, agents, skills, pending prompts)
    let boot = sdk.bootstrap().await?;
    println!("Connected! Models available: {}", boot.models.len());

    // 2. Create a session in the scoped directory
    let session = sdk.create_session("hya-main", "hya/offline").await?;

    // 3. Open the live SSE stream BEFORE submitting the turn
    let mut stream = sdk.stream_session(&session.id, None).await?;
    let mut mirror = SessionMirror::from_session(&session, Vec::new());

    // 4. Submit a prompt turn
    let _turn = sdk.prompt(&session.id, "Summarize Cargo.toml").await?;

    // 5. Fold StreamFrames until the assistant message finishes
    while let Some(frame) = stream.next().await {
        let frame = frame?;
        mirror.apply(&frame);
        if frame.is_turn_finished() {
            break;
        }
    }

    // Inspect folded bundle extension state (`Projection::extensions`) and events
    if let Some(wf) = mirror.extension("hya/workflow") {
        println!("Workflow extension state: {wf}");
    }

    Ok(())
}
```

### Handling reconnects and `resync`

`stream_session` opens a single SSE connection without hidden background retries, keeping your UI state predictable:
- If the stream yields a **`resync`** frame (meaning your client read slower than the 8,192-event ring buffer) or reconnects after a network blip, re-fetch durable messages (`GET /v2/sessions/{id}/messages`) and session metadata (`GET /v2/sessions/{id}` to re-seed `SessionInfo.extensions`) or replay missed durable events from your last seen `seq > 0` watermark (`sinceSeq`).
- If the stream yields **`serverStopping`**, check `reason`: on `"restart"`, wait for the new daemon URL in `<db>.server.json` and reconnect; on `"stop"`, show a disconnected indicator until the user reconnects.

---

## 2. Direct gRPC Clients (`hya-api` / `proto/hya/v2`)

Prefer gRPC over HTTP/JSON? Point any tonic, Go, Python, or Connect/gRPC client (HTTP/2 cleartext `h2c` locally, or over the encrypted relay) at the **same `http://127.0.0.1:<port>` address** using the `.proto` definitions in [`proto/hya/v2/`](../../proto/hya/v2/).

In Rust workspace crates, `hya_api::v2` exports all generated `prost` messages and `tonic` client stubs out of the box.

---

## 3. Lightweight Shell / TypeScript / Python Integration

If you don't even want to manage an HTTP server connection, remember you can also spawn `hya exec --json` as a subprocess and read one committed event per line from `stdout`:

```sh
hya exec --json "Find unused dependencies in package.json"
```

When you *do* want persistent sessions, live multi-client sync, bundle APIs, or interactive permission approvals from a custom web/desktop frontend, connect to `hya serve start --json` (parse its `url` field) and speak `/v2` HTTP + SSE directly:

```ts
// Minimal browser / Bun / Node fetch + EventSource pattern
const base = "http://127.0.0.1:8080"

const { session } = await fetch(`${base}/v2/sessions`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ agent: "hya-main", model: "hya/offline", workdir: "/tmp" }),
}).then((r) => r.json())

const events = new EventSource(`${base}/v2/sessions/${session.id}/events/stream`)
events.onmessage = (e) => {
  const frame = JSON.parse(e.data)
  console.log("Frame:", frame)
}

await fetch(`${base}/v2/sessions/${session.id}/turns`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ prompt: { text: "Hello from TypeScript!" } }),
})

// Invoke a native or JS bundle endpoint via BundleApi:
const workflows = await fetch(
  `${base}/v2/bundles/hya%2Fworkflow/api/workflows?directory=/tmp`,
).then((r) => r.json())
console.log("Workflows:", workflows)
```

> **Remote Relay note:** If your custom client connects through a local `hya bridge --json`, pass the printed `token` on every HTTP/SSE request as `x-hya-bridge-token: <token>`, and make sure you send requests from a native/Node/Bun process rather than directly from a third-party browser `Origin` (browser `Origin`/`Sec-Fetch-*` headers are blocked over the relay to protect remote shells from malicious web pages).
