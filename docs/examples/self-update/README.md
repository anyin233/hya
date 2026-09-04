# Self-update example (local dry-run)

This example shows the **product path** introduced in `0.34.13`, exercised
against the checked-out updater at workspace version `0.36.11` without claiming a
production release or archive payload:

1. Build `hya-updater`.
2. Create a temporary updater root and package directory.
3. Sign fixture metadata with a throwaway key (demo only).
4. Run `apply` without `--owner-authorized-activation`; this verifies and stages
   the candidate but does not switch `current` or advance `accepted_floor`.
5. Run `status`, then **discard** the staged sequence.
6. Re-run `apply` with `--owner-authorized-activation` under the same temp root;
   this commits the selector and advances the floor.

Do **not** reuse the demo key material for real releases.

```sh
# from repo root
./docs/examples/self-update/run-demo.sh
```

Expected outcome: stage succeeds, `status` reports `current_sequence=0` and
`accepted_floor=0`, discard removes the unselected sequence, and the
owner-authorized apply reports `current=1` with `accepted_floor=1`.
