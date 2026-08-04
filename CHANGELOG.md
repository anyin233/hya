# 0.34.13

- Add independent `hya-updater` trust boundary: ed25519-signed release metadata
  verification (trust root, sequence anti-rollback floor, platform, freshness,
  artifact digests) with no `hya-core` / plugin / MCP / bundle / app / store
  dependencies.
- Stage immutable versioned releases under `releases/<sequence>/`, smoke
  candidates in a dedicated subprocess (no in-process candidate load), and
  commit activation through prepare journal + atomic selector rename + accepted
  floor advance.
- Recover interrupted prepare states to one complete generation; abort keeps the
  previous selector and never decrements the accepted floor. Higher-sequence
  signed recovery may reinstall old bits only by advancing the floor.
- Keep production update activation owner-gated; `install.sh` remains break-glass
  bootstrap/recovery.
