# Self-update example (local dry-run)

This example shows the **product path** for 0.34.13 without claiming production
activation:

1. Build `hya-updater`.
2. Create a temporary updater root and package directory.
3. Sign fixture metadata with a throwaway key (demo only).
4. `apply` without `--owner-authorized-activation` (stage only).
5. Optionally re-apply with the owner flag under the same temp root.

Do **not** reuse the demo key material for real releases.

```sh
# from repo root
./docs/examples/self-update/run-demo.sh
```

Expected outcome: stage succeeds, floor stays `0` until the owner flag is used;
with the flag, `current` and `accepted_floor` advance to `1`.
