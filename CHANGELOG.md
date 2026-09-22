# 0.37.8

## Native executable bundle packaging

- Allow `extensions.rust` to carry raw executable bytes for a `kind: rust` process. Verify their original-byte digests, canonical prepared encoding, and exact public package closure.
- Materialize declared native executables with private executable permissions before process startup; retain atomic publication and old-binding lifetime behavior.
- Document the native executable manifest and package workflow.
