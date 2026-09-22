# 0.37.14

## TODO native tool bundle

- Move the three TODO tool implementations into the TODO bundle's Rust dynamic library while keeping session TODO state in the tool interface crate.
- Package raw native libraries in public bundles and load trusted first-party family packages with a lockstep ABI check.
- Ship the TODO library package beside the backend in release assets.
