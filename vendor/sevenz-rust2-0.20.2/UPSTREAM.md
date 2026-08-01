# Upstream provenance

- Crate: sevenz-rust2 0.20.2
- Upstream commit: 424ebdb8fa98b78b8e1c18f73c9add6972fe5496
- Official crates.io checksum: 29225600349ef74beda5a9fffb36ac660a24613c0bde9315d0c49be1d51e9c24
- License: Apache-2.0

This vendored Cargo manifest omits absent example declarations and adds an empty
workspace boundary solely for nested-worktree Cargo isolation. Its compact
security-focused reader patch adds bounded metadata-reading options, validates
decoder limits and strict allowed codecs before decoding. Hya uses the strict
reader to decode the single accepted
`bundle.hya.md` stream into its own bounded in-memory buffer, never the stock
filesystem extractor; the patch adds bounded metadata/decoder/security checks
and no second parser or filesystem output path.

Functional patch files: src/reader.rs, src/decoder.rs, src/error.rs, and
src/lib.rs.
