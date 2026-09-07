<!--
  Built-in skill. Name and description are registered in
  skill_catalog.rs. The body below becomes the skill content.
-->

# Secure self-update

Use this skill when verifying, staging, recovering, or owner-activating an
independent hya release with `hya-updater` (0.34.13+). Do **not** use it for
bundle install, plugin load, or ordinary `install.sh` source installs unless the
user is comparing break-glass recovery.

## Hard rules

- The updater TCB must not depend on runtime, plugin, MCP, bundle, app, or session DB code.
- Signatures alone never activate. Production activation needs explicit owner authorization (`--owner-authorized-activation` / `owner_authorized`).
- Network download is outside the TCB. Download a complete package directory first; pass a local path or `file://` URL.
- Never lower `accepted_floor`. Recovery of older bits requires a new higher signed sequence.
- `install.sh` remains break-glass bootstrap/manual recovery.

## References

- Guide: `docs/self-update.md`
- Example: `docs/examples/self-update/`
- Crate: `crates/hya-updater`

## Operator flow

1. Ensure `trust_roots.json` exists under the updater root.
2. Obtain signed `release.metadata.json` and a local package directory of artifacts.
3. `hya-updater apply --root … --metadata … --package … --platform … [--smoke smoke.sh]` for stage-only.
4. On success and owner approval: re-run with `--owner-authorized-activation`.
5. On failed smoke before activation: `hya-updater discard --root … --sequence N`.
6. On crash mid-update: `hya-updater recover --root …` then `status`.

## Agent boundaries

Do not invent signing keys, waive anti-rollback, load candidate code in-process,
or claim production activation without documented owner authorization.
