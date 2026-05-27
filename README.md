# letswrite

A focused, Markdown-first writing app for novelists. Built in Rust with [Iced](https://iced.rs).

## Status

Early development. See [`docs/tasks.md`](docs/tasks.md) for the roadmap.

## Layout

- `crates/letswrite-app` — Iced UI (binary)
- `crates/letswrite-core` — data model, persistence, services
- `crates/letswrite-import` — Obsidian vault importer
- `crates/letswrite-ai` — AI assistant abstraction + provider implementations

## Build

```
cargo build
cargo run -p letswrite-app
```

Requires a recent stable Rust toolchain (see `rust-toolchain.toml`).
