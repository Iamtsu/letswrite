# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

letswrite is a desktop book-writing app (Rust + Iced) being built incrementally and dogfooded by the maintainer on a real novel at `/home/tsu/Projects/private/The-Threshold` (an Obsidian vault that will be imported in #8). It is a shippable product, not a prototype — see `docs/tasks.md` for the phased roadmap.

## Common commands

```bash
cargo build                                                 # build everything
cargo run -p letswrite-app                                  # run the UI
cargo test --workspace                                      # run all tests
cargo test -p letswrite-core settings::tests::               # run one test module
cargo clippy --workspace --all-targets -- -D warnings        # lint gate (CI bar)
cargo fmt --all                                             # format
RUST_LOG=letswrite=debug cargo run -p letswrite-app          # verbose logs
```

`-D warnings` is the standard — workspace lints enable `clippy::pedantic` and `clippy::nursery`, so new code must pass clippy with warnings denied. When a pedantic/nursery lint genuinely doesn't apply (e.g. Iced's `update(&mut self, Message)` contract trips `needless_pass_by_value`), add a narrowly-scoped `#[allow(...)]` with a one-line note rather than disabling the lint workspace-wide.

## Architecture

**Four crates, layered:**

- `letswrite-app` — Iced UI binary. The *only* crate that may depend on Iced or talk to UI concerns. Imports `letswrite-core` and (later) `letswrite-ai`/`letswrite-import`.
- `letswrite-core` — UI-independent domain model: settings, i18n, errors, and (incoming) persistence, documents, entities. Pure logic; no Iced, no async runtime assumptions.
- `letswrite-import` — Format importers (Obsidian first). Depends on `letswrite-core`.
- `letswrite-ai` — AI assistant abstraction. See "AI abstraction" below — its public surface is a hard contract.

The dependency graph is strictly `app → {core, import, ai}` and `import → core`. Do not introduce back-edges.

**Two-tier AI abstraction (load-bearing):**

The AI layer is split into `Provider` (low-level, vendor-specific) and `Agent` (high-level, UI-facing) traits. The UI talks to `Agent` only and must never import a `Provider` impl directly. Anthropic is the first `Provider` implementation but is not the contract — vendor specifics (SSE event names, model IDs, prompt caching headers) stay inside `letswrite-ai::providers::anthropic` and never leak into `Agent`, `AssistantContext`, or the UI. Errors from a `Provider` map to a small abstraction-level enum (`AuthError`, `RateLimited`, `Transient`, `ProtocolError`, `Transport`); higher layers don't see raw HTTP status codes. Cancellation is via `tokio_util::sync::CancellationToken` end-to-end. Credentials live in the OS keyring (`keyring` crate) and are never logged or written to disk in plaintext.

The trait definitions land in task #12; the Anthropic impl in #30. Build #12 to completion with a `MockProvider`, write the UI/context layers against the abstraction, then build #30 — don't leak Anthropic types upward to unblock yourself.

**Filesystem-first persistence (planned, #4):**

Markdown files on disk are the source of truth for prose. SQLite (at `.letswrite/db.sqlite` inside each project) is an index/cache for entities, scenes, relationships, snapshots, AI threads. This means a project survives the app dying and stays git-friendly. A `notify`-based watcher will re-sync SQLite when files change outside the app.

**Settings (`letswrite-core/src/settings.rs`):**

- TOML at the OS-standard config path (`directories::ProjectDirs`).
- Atomic writes via tempfile + rename — a crash mid-write can't corrupt settings.
- `#[serde(default, deny_unknown_fields)]` on every struct: old files keep loading (forward-safe), unknown fields error loudly (typo-safe).
- When adding fields: give them `Default` impls; never remove or rename without a migration.

**i18n (`letswrite-core/src/i18n.rs`):**

- Fluent (`.ftl`) bundles. English bundle is `include_str!`'d as the always-available fallback (`crates/letswrite-core/i18n/en/letswrite.ftl`); other languages load from disk at runtime via `I18n::load_extra`.
- Lookup order: requested language → English fallback → `‹key›` marker (so missing translations are visible, not silent).
- BCP-47 negotiation: `pt-BR` falls back to `pt` then `en` automatically.
- All user-visible strings go through `i18n.tr("key")` — never hardcode UI text.
- Pass user content (especially anything bound for the AI) through verbatim; avoid ASCII assumptions in tokenization/length code (use `unicode-segmentation`).

**Iced UI patterns (`letswrite-app/src/app.rs`):**

- `pane_grid::PaneGrid` for the three-column shell (sidebar | editor | assistant) with drag-resizable splitters. Split ratios persist to settings on resize.
- Pane closures cannot borrow from `view()` locals — `pane_grid::Content<'a>` is invariant in `'a`. Resolve strings (e.g. via `i18n.tr`) into owned `String` *before* the closure and `.clone()` per pane.
- Each pane container needs an explicit opaque background (`pane_surface_style`) — without it, the splitter-gap color bleeds across the whole window.
- `iced::application(...)` is invoked from `main.rs` with `.window_size(...)` from settings, `.theme(App::theme)`, and `.run_with(App::new)`.

## Error handling

Top-level `letswrite_core::Error` (in `error.rs`) is intentionally coarse. Use `Error::io_at(path, source)` instead of bare `Io(source)` so file errors carry the path. New variants should be added only when callers actually need to distinguish them — don't pre-emptively split.

## Task tracking

`docs/tasks.md` is the cross-session checkpoint of the dependency-wired roadmap; the in-session `TaskList` is the live tracker. When work on a task starts, mark it `in_progress`; when complete, mark it `completed` AND tick the matching checkbox in `docs/tasks.md`. The user's global instructions require this — see `~/.claude/CLAUDE.md`. Project-level decisions and conventions also live in memory at `/home/tsu/.claude/projects/-home-tsu-Projects-private-letswrite/memory/`; check `MEMORY.md` there at session start.

## Conventions

- Avoid `pub` on items in the `letswrite-app` binary crate — workspace lints set `unreachable_pub = "warn"`. Use `pub(crate)`. In the bin crate, this conflicts with `clippy::redundant_pub_crate`; the conflict is silenced via crate-level `#![allow(clippy::redundant_pub_crate)]` in `main.rs`.
- `unsafe_code = "forbid"` workspace-wide.
- Default to writing no comments (per global instructions). Add a one-liner only when the *why* is non-obvious — hidden constraints, subtle invariants, workarounds. Don't narrate what the code does.
