# Upstream Boundary And License Research

## Source Record

- Repository: `https://github.com/anomalyco/opencode`
- Local checkout: `/chivier-disk/yanweiye/Projects/opencode-frontent-rs/opencode-origin`
- Version: `1.17.9`
- Commit: `cf31029350820c6bfc0fbd0e052a79a067ee6116`
- Primary package: `packages/tui`
- Runtime: Bun `1.3.14`, OpenTUI `0.3.4`, SolidJS `1.9.10`

## License Finding

The root license is MIT and states `Copyright (c) 2025 opencode`. Its condition
requires the copyright and permission notice to be included in copies or
substantial portions. The migration must therefore carry that complete text in
the derived package and every distributed copy. A separate provenance notice is
not mandated by MIT but is included to make the imported boundary and hya
modifications clear.

## Runtime Seam

The reusable frontend seam is the TUI `run(TuiInput)` entry and `SDKProvider`:

- base URL and project directory are inputs;
- `@opencode-ai/sdk/v2` performs HTTP requests;
- `/global/event` supplies streamed events;
- an optional custom fetch/event source exists for OpenCode's worker mode but
  is not required for direct HTTP operation.

The OpenCode CLI `TuiThreadCommand`, worker RPC, server startup, updater, heap
snapshot server coupling, and backend plugin host are outside the frontend
boundary.

## Reachable External Package Needs

The TUI source imports:

- `@opencode-ai/sdk/v2` for API types and client behavior;
- `@opencode-ai/plugin/tui` for TUI contracts and keymap helpers;
- `@opencode-ai/core` for a small set of paths, flags, version, lock, glob, and
  executable lookup utilities;
- `@opencode-ai/ui` only for TUI attention audio assets.

The core utilities are not a reason to import OpenCode core. They can be
replaced with Node/Bun platform APIs and hya paths. Required audio files may be
copied as attributed TUI assets. Static built-in components may use the plugin
contract, but the external plugin loader and package manager are excluded.

## OpenCode-Only Features To Remove

- update availability and `global.upgrade` actions;
- OpenCode docs, issue, Zen, Go, and product links;
- Console organization state;
- share/public URL behavior;
- remote workspace management and synchronization;
- dynamic TUI plugin add/install/activate management;
- OpenCode provider promotions and OAuth assumptions not backed by hya.

## Branding Inventory

User-visible occurrences exist in terminal titles, ASCII logo, status command,
default theme/sound IDs, help/tips, config paths, docs/provider/update links,
error reporting, and temporary filenames. Internal SDK imports, generated type
names, protocol identifiers, source comments, legal notice, and this provenance
record are not product branding.

## Sync Policy

The first import records one exact upstream commit and separates derived source
from hya-owned adapters. No sync command or generic vendoring framework is added
until a second upstream update demonstrates the required workflow.
