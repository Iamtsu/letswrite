//! Concrete `Provider` implementations.
//!
//! Each provider module is fully self-contained: vendor-specific wire
//! shapes, SSE event names, model IDs, header conventions all live here.
//! They map up to the abstraction-level [`crate::Provider`] trait.

pub mod anthropic;

pub use anthropic::AnthropicProvider;
