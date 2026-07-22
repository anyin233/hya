# 0.33.29

- Made the SolidJS/OpenTUI frontend the default `hya` TUI through the adjacent `hya-ts` launcher while retaining the `hya-ts` alias and Compat config import.
- Removed the shipped Rust TUI entrypoint and routed bare `hya-backend` startup through the canonical frontend.
- Added installed and release-archive startup checks for the default frontend chain.
