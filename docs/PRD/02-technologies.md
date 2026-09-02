## Technologies

The project is written in Rust. Rust provides type and memory safety without a garbage collector, which suits a long-lived, performance-sensitive tool.

**Toolchain.** The workspace targets Rust **edition 2021**; the MSRV is the latest stable release at the time of development (pinned in `rust-toolchain.toml`). JSON serialization keeps object-key insertion order via `serde_json` with the `preserve_order` feature (backed by `indexmap`). YAML is parsed with `yaml-rust2`, preserving line/column spans on every scalar for diagnostics.
