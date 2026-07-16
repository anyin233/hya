# Rust plugin binaries over in-process libraries

Status: accepted

hya's native plugin boundary is a Rust plugin binary: an external process that communicates with the runtime through hya's JSON-RPC 2.0 newline-delimited stdio protocol and is adapted into the shared `Tool` registry. This follows the existing `hya-plugin` shape (`PluginHost`, `PluginConn`, protocol frames, and `PluginTool`) and keeps private plugin distribution independent of Rust's unstable in-process ABI.

Considered alternatives: in-process `cdylib` plugins would avoid process startup but would couple plugin authors to runtime ABI/compiler details and let plugin crashes corrupt the host; WASM-first plugins would improve sandboxing but would force a separate host ABI before hya has exhausted the simpler process/protocol boundary; JS/TS Compat plugins remain supported through the Compat adapter but are not the native private-distribution path.
