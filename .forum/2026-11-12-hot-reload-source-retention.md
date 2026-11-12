# How do hot-reload failures retain sources without pinning retired plugins?

Treat attempted input and published source as separate state. A failed config
plugin, Project plugin, Project bundle, or MCP preparation keeps its previous
published tools and hooks. Automatic binds remember the attempted input and use
the retained catalog. An explicit refresh retries failures with unchanged
inputs; independent successful sources still publish and emit a catalog notice.
Unreadable declarations remain failed inputs, rather than becoming deletions.
A malformed config file must still allow explicit retries of previously known
failed MCP declarations.

User-global plugin hooks run before installed plugin bundles and Project plugins.
Active turn bindings retain their global hooks. Idle session captures contain
only scoped hooks; global events and lifecycle hooks use the currently published
base sources. Base publication clears cached Project compositions without
removing overlays, so cached snapshots cannot pin removed global plugins.
Weak snapshot references keep active bindings visible to scope eviction.
Project plugin retry records store source IDs, not process owners.

Global event delivery belongs only to `publish_envelope`, unconditionally.
Activation chains retain global plugins with event delivery suppressed;
captured chains exclude them. A resident's initial registration prompt precedes
its sidecar, so reproducing a pre-turn omission requires a later mail wake.

Project bundle precedence uses manifest id and normalized namespace before
reading resources or preparing processes. Shadowed directories contribute no
refresh error and release any previously published source. Failed unshadowed
manifests keep their retained identity's precedence.

Resolve MCP secret expressions at source preparation, including when enabling a
previously disabled declaration. Compare declarations after normalizing default
enabled, environment, transport, and timeout values. The refresh HTTP response
uses protojson: uint64 generations are strings and an empty `errors` array is
omitted. CLI clients must inspect the typed response body even after HTTP 200.
