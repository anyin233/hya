# 0.46.1

This release combines 0.46.0 with the parallel 0.45.11 line. The headless
continuation, time-budget guidance and Bash/wait timeout caps, native TLS
roots alongside bundled web roots, and revised agent prompts were developed
in parallel and are already part of 0.46.0's code; this merge reconciles their
history rather than introducing those behaviors again. See the archived
[0.45.11 notes](docs/changes/CHANGELOG_BACKEND_0.45.11.md).

The refreshed documentation index, quick start, configuration guides and
architecture diagrams describe the resident-episode runtime, channel-backed
mail, active-claim capacity, and stop/start recovery. Obsolete compatibility,
legacy TUI, and superseded restart documents remain removed.

Upgrading from 0.45.x requires following the
[0.46.0 breaking-change notes](docs/changes/CHANGELOG_BACKEND_0.46.0.md),
including starting with a fresh database and reinstalling bundles.
