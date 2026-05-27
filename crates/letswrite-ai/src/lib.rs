//! AI assistant abstraction for letswrite.
//!
//! Two-tier design:
//! - [`Provider`] is the low-level, vendor-specific contract (one impl per backend).
//! - [`Agent`] is the high-level, UI-facing contract that wraps a Provider with
//!   conversation state and context-assembly strategy.
//!
//! See `docs/tasks.md` (#12) for the full design.

// Trait/type definitions land in #12. This module exists today to anchor the
// dependency graph for the UI crate.
