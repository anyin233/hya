# Foundation and implementation sequence

This page separates plugin responsibilities from reusable host facilities. The
implemented contract is in [README.md](README.md); interfaces below are proposals,
not available routes or methods.

## 1. Connect discovery through a generic host bridge

The existing backend already routes bundle APIs. The current TUI SDK has no
own-bundle request operation. Add one permission-controlled host facility that
uses the frontend's existing HTTP/gRPC client and active backend connection.
It must bind requests to the owning bundle and surface transport, host, and
provider errors. Do not let a pane choose another bundle or an arbitrary URL.

Adapt that facility to `InspectorClient.info()`. Load provider discovery outside
render callbacks, keep the latest state, and invalidate the affected pane when
the result changes. The frontend must display the returned backend machine
identity. Never substitute local `api.fs` data on a remote connection.

## 2. Implement the backend scanner

Add bounded background scan jobs to this plugin, independently of model turns.
Proposed endpoint set:

| Proposed endpoint | Request | Response |
| --- | --- | --- |
| `GET /volumes` | No body | Volume identities, mount points and byte totals |
| `POST /scans` | Root and scan options | Scan id and initial progress |
| `GET /scans/{id}` | Scan id | Bounded snapshot, status and errors |
| `POST /scans/{id}/cancel` | Scan id | Cancellation acknowledgement |

Define exact schemas before exposing these routes. Use byte counts that preserve
integer precision across JSON (decimal strings where necessary). Distinguish
apparent and allocated size, filesystem totals and folder totals, mount boundaries,
hard-link accounting, unreadable paths, and partial results. Do not follow symlinks
by default. Bound concurrent scans and traversal memory. Cancellation must stop
work promptly, including when the provider shuts down.

Resolve scan roots and cache directories explicitly on the backend: bundle
processes do not inherit an ordinary shell environment. Keep any scan snapshots,
cache, build output, and other bulky artifacts under configured `~/data` storage.
The first inspector is read-only; deletion and cleanup are separate features.

## 3. Make the plugin pane useful

Add volume selection, directory navigation, progress and cancellation to the
plugin's view model and renderer. Register keys/actions through the public SDK
and existing focus routing. Perform RPCs from controller actions; rendering reads
cached snapshots. Poll progress on a bounded interval until the host offers a
general subscription facility. Do not require a subscription redesign to show
the first useful scan.

## 4. Support multiple independent pane instances

The current host identifies callbacks and caches by `bundle#panel`. Add a generic
pane instance id and optional per-instance settings before supporting independent
inspectors for different roots. Keep definitions, instances and backend jobs
separate. A closing pane releases its owned polling/subscriptions; plugin unload
also disposes provider work. Decide cache/job lifetime explicitly.

## Acceptance checks for integration

- Discovery uses the active backend and works through HTTP and direct gRPC.
- A remote inspector reports the remote machine's disks.
- Permission denial, disabled provider, disconnect and reconnect are visible.
- Long scans do not block key input or render callbacks.
- Closing/reloading a pane does not leave polling or jobs running unintentionally.
- Tests cover sparse files, hard links, symlinks, mount boundaries and partial scans.
- Browser PTY tests cover placement, focus, resizing, errors and closing; inspect
  their screenshots at standard and narrow sizes.
