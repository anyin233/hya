# 0.43.4

## Hot-reload completion

- Hardened updater activation with an owner lease, authorization capability,
  generation fencing, crash recovery, and atomic selector commits.
- Preserved pending permission and question requests across daemon handoff,
  including durable replies and successor continuation recovery.
- Added reconnect/resync behavior for SDK and HTTP clients and documented the
  process boundary for streams and provider connections.
