# Disk inspector plugin

The disk inspector is an optional plugin with a backend provider and a frontend
pane, developed under [`plugins/disk-inspector`](../plugins/disk-inspector/README.md).
It exists to exercise the public extension contracts while keeping filesystem
inspection outside the harness's layout and rendering code.

The initial scaffold exposes provider discovery and an opt-in disconnected pane.
It does not scan disks yet. Its [README](../plugins/disk-inspector/README.md)
documents installation, checks, layout commands and exact implemented interfaces.
Its [foundation plan](../plugins/disk-inspector/FOUNDATION.md) describes the generic
host bridge and subsequent scanner and pane work.
