# 0.34.9

- Add content-magic inspection for public and private bundle packages.
- Add a strict, bounded pure-Rust 7z reader/profile and canonical v1
  public-package preparation with a fixed 1000:1 expansion ceiling using the
  accepted block's referenced PackInfo stream sizes, enforced at metadata
  preflight and before retaining each decoded chunk.
- Keep private package inspection structural only: authentication remains unverified,
  payloads remain opaque, and inspection does not activate package content.
- Add the SQLite prepared-BLOB registry core with immutable builtins,
  idempotency, conflict/replacement/uninstall handling, and atomic generation
  updates.
