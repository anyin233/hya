# Disk inspector plugin

A standalone Hya plugin for inspecting disk space on the **backend machine**.
It owns both the scanning provider and its frontend pane. The harness supplies
installation, transport, layout, focus, and rendering through public contracts.

## Current status

This is the initial runnable scaffold. It packages successfully and exposes
provider discovery. Its optional pane explicitly reports that the frontend
bundle API bridge is missing. It does **not** scan disks, report fabricated
usage, or access the frontend's filesystem. Scanning, cancellation, and volume
queries are subsequent plugin work; the generic bridge is subsequent host work.

## Folder ownership

```text
disk-inspector/
├── bundle.yaml          # One installation, backend process + frontend entry
├── package.json         # Local checks and builds
├── tsconfig.json        # Development SDK type resolution
├── backend/
│   ├── main.ts          # NDJSON JSON-RPC process entry
│   └── provider.ts      # Discovery and request dispatch
├── shared/
│   └── contracts.ts    # Plugin DTOs and client interface
├── tui/
│   ├── main.ts          # Public SDK registration
│   └── panel.ts         # Presentation, without OS or network access
├── schemas/
│   └── info.json        # Implemented discovery response schema
├── test/                # Provider process and SDK registration tests
└── FOUNDATION.md        # Host requirements and next implementation phases
```

Runtime relative imports stay inside this directory. The only frontend external
import is the host-provided `@hya/tui-sdk`. Development references the checkout's
SDK; that TypeScript path mapping is not packaged runtime code.

## Usage

From the repository root, with the existing SDK development dependencies:

```sh
cd plugins/disk-inspector
../../packages/hya-tui-sdk/node_modules/.bin/tsc --noEmit
bun test ./test
bun run build
```

Build output is under `~/data/hya-plugins/disk-inspector/build/`. There is no
scan cache yet. Future persistent scan data must also default under `~/data`.
With this package's development dependencies installed, `bun run typecheck`
is equivalent to the explicit compiler command above.

The integration spec uses the existing browser PTY harness and the real package:

```sh
# From the repository root; build hya and the packaging executable if necessary.
cargo build -p hya-backend --bin hya
cargo build -p xtask
mkdir -p "$HOME/data/hya-plugins/disk-inspector/tmp"
cd packages/hya-tui-web
TMPDIR="$HOME/data/hya-plugins/disk-inspector/tmp" \
  PLAYWRIGHT_BROWSERS_PATH="$HOME/data/hya-rust/playwright-browsers" \
  PLAYWRIGHT_HTML_OUTPUT_DIR="$HOME/data/hya-plugins/disk-inspector/report" \
  bunx playwright test e2e/hya-tui-disk-inspector.spec.ts \
  --output="$HOME/data/hya-plugins/disk-inspector/test-results"
```

To run the provider directly:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1}}' \
  '{"jsonrpc":"2.0","id":2,"method":"api/request","params":{"api":"info","method":"GET","path":"/info"}}' \
  '{"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}' \
  | bun run backend/main.ts
```

To package and install, from the repository root:

```sh
mkdir -p "$HOME/data/hya-plugins/disk-inspector"
cargo run -p xtask -- package-bundle plugins/disk-inspector \
  "$HOME/data/hya-plugins/disk-inspector/disk-inspector.hyabundle"
target/debug/hya bundle install \
  "$HOME/data/hya-plugins/disk-inspector/disk-inspector.hyabundle"
```

In the TUI, check `/extensions`, then place the pane explicitly:

```text
/layout split left extension hya-extra/disk-inspector#disk
```

The pane is opt-in (`placement: pane`). It does not add a default sidebar or
claim keyboard bindings. It needs only `tui.panel`. The current displayed
message is “Disk inspector is not connected.” Installing the bundle does not
make the missing host bridge available.

## Implemented interface definitions

Identity: `hya-extra/disk-inspector`; provider protocol id: `disk-inspector`;
pane key: `hya-extra/disk-inspector#disk`. Manifest identity version is `0.0.0`
with `version_ref: backend`, following the existing bundle preparation contract.
This optional source folder is not part of Hya's release archives.

### Backend

The process speaks [plugin protocol v1](../../docs/plugin-protocol.md) using
one JSON-RPC 2.0 object per line. It implements:

| Method | Parameters | Result |
| --- | --- | --- |
| `initialize` | `{protocol_version: 1, host?: object}` | Protocol version, Bun provider identity, empty tools/hooks, `apis: [{name: "info", description: string}]` |
| `api/request` | `{api: "info", method: "GET", path: "/info", ...host fields}` | `{status: 200, body: InspectorInfo}` |
| `shutdown` | `{}` | `{}`, flushed before the process exits |

The manifest exposes exactly one global endpoint:

```text
GET /v1/bundles/hya-extra%2Fdisk-inspector/api/info
```

It takes no plugin-specific body or query parameters. Standard host fields such
as `call` and `host_capability` are accepted but unused; this endpoint calls no
host capabilities. There are no tools and no hooks.

```ts
interface InspectorInfo {
  contractVersion: 1;
  machine: { hostname: string; platform: string };
  capabilities: { volumes: false; scans: false; cancel: false };
}
```

The exact response JSON Schema is [schemas/info.json](schemas/info.json).
Wrong API ids, methods, or paths return status `404` with
`{error: {code: "endpoint_not_found", message: string}}`. Malformed JSON returns
JSON-RPC `-32700`; malformed requests return `-32600`; unsupported initialization
versions return `-32602`; unknown RPC methods return `-32601`. Valid notifications
have no reply. Standard host body/frame/time limits still apply.

### Frontend

The SDK entry registers `{id: "disk", title: "Disk inspector", placement: "pane"}`.
`renderInspector(state)` returns a declarative SDK `RenderNode`. Its input is:

```ts
type InspectorState =
  | { kind: "disconnected" }
  | { kind: "loading" }
  | { kind: "failed"; message: string }
  | { kind: "ready"; info: InspectorInfo };

interface InspectorClient {
  info(): Promise<InspectorInfo>;
}
```

`InspectorClient` is a plugin-local adapter interface, not an existing SDK API.
The installed entry stays disconnected until a real host transport is connected.
See [FOUNDATION.md](FOUNDATION.md) for the next steps.
