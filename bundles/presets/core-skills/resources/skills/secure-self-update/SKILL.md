---
name: secure-self-update
description: >-
  Use when verifying, staging, recovering, or owner-activating an independent hya release with `hya update`: signed metadata, local package fetch, immutable staging, smoke subprocess, activation journal/selector, anti-rollback floor, and install.sh break-glass. Do not use for bundle install, plugin load, or to skip the owner activation gate.
---
<!-- Trusted core Skill; its metadata and body are prepared from this bundle. -->

# Secure self-update

Use this skill when verifying, staging, recovering, or owner-activating an
independent hya release with `hya update` (0.38.0+; formerly the `hya-updater` binary). Do **not** use it for
bundle install, plugin load, or ordinary `install.sh` source installs unless the
user is comparing break-glass recovery.

## Hard rules

- The updater TCB must not depend on runtime, plugin, MCP, bundle, app, or session DB code.
- Signatures alone never activate. Production activation needs an explicit owner capability (`--authorization PATH`) bound to the exact candidate sequence and expected active generation.
- The capability is a trusted-filesystem handoff, not a same-UID sandbox; processes able to write the updater root share its trust boundary.
- Never lower `accepted_floor`. Recovery of older bits requires a new higher signed sequence.
- `install.sh` remains break-glass bootstrap/manual recovery.

## References

- Guide: `docs/self-update.md`
- Example: `docs/examples/self-update/`
- Crate: `crates/hya-updater`

## Operator flow

1. Ensure `trust_roots.json` exists under the updater root.
2. Obtain signed `release.metadata.json` and a local package directory of artifacts.
3. `hya update apply --root … --metadata … --package … --platform … [--smoke smoke.sh]` for stage-only.
4. The trusted owner calls `UpdaterOwner::authorize` for the candidate and expected generation, writes the capability JSON, then re-runs with `--authorization capability.json`.
5. On failed smoke before activation: `hya update discard --root … --sequence N`.
6. On crash mid-update: `hya update recover --root …` then `status`.

## Source-checkout flow (rebuild and restart)

To replace the running backend with code you changed in a hya source checkout
(no signed release involved):

1. Build: `cargo build -p hya-backend --bin hya`.
2. Restart from the new build with the tests that prove the change:
   `./target/debug/hya serve restart --verify 'cargo test -p <crate>'`.
   The restart first runs `hya serve check` of the new build against a snapshot
   of the live database; a failing check or verify command leaves the running
   backend untouched — fix and retry.
3. The current turn resumes in the new build; the next turn runs the new code.
4. `hya serve status`: a `restart` line means the new build failed to start and
   the backend rolled back to the previous build; read `<db>.server.log`.

## Agent boundaries

Do not invent signing keys, waive anti-rollback, load candidate code in-process,
or claim production activation without documented owner authorization.
