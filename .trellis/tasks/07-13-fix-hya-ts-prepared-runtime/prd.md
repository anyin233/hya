# Fix hya-ts prepared runtime

## Goal

Repair the installed hya-tui-ts runtime after SDK server pruning, ship required Bun/TypeScript config, and release as 0.33.1.

## Requirements

- A prepared production runtime must import `createOpencodeClient` from the
  pinned SDK after its OpenCode server launcher is removed.
- The prepared runtime must contain the Bun/OpenTUI preload and TypeScript JSX
  configuration used when `hya-ts` executes `src/main.tsx`.
- Installer and release staging must produce the same runtime contract and
  reject a broken SDK import before installation or publication.
- Preserve the existing `hya`, `hya-backend`, and `hya-ts` transaction,
  attribution files, Bun requirement, and rollback behavior.
- Publish no release or tag. Version this post-`0.33.0` fix as `0.33.1`.

## Acceptance Criteria

- [ ] A fresh staged copy of `@opencode-ai/sdk@1.17.9` imports the v2 client
      after pruning, while server/process barrels and exports are absent.
- [ ] Installed and release runtime layouts contain `bunfig.toml`,
      `tsconfig.json`, source, lockfile, dependencies, license, and provenance.
- [ ] Installer and release smoke checks execute the staged SDK client import.
- [ ] Focused runtime/installer tests, Bun checks, Rust workspace gates, local
      executable build, and diff/status checks pass.
- [ ] Cargo, TypeScript package, and newest changelog versions are `0.33.1`;
      the exact `0.33.0` notes are archived.

## Notes

- Root cause reproduced from the installed `0.33.0` runtime: pruning deleted
  `dist/v2/server.js`, but the retained `dist/v2/index.js` still imported it.
