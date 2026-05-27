//! letswrite — a Markdown-first writing app for novelists.

// Binary crate: nothing is exported. `unreachable_pub` and `redundant_pub_crate`
// contradict each other in this context, so we silence the latter.
#![allow(clippy::redundant_pub_crate)]

mod app;
mod assistant;
mod context_builder;
mod editor;
mod presets;
mod sidebar;
mod syntax;
mod views;

use iced::Size;
use letswrite_core::Settings;
use tracing_subscriber::EnvFilter;

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting letswrite");

    // Probe settings once for the initial window size. The App will re-read
    // and own settings during startup. f32 mantissa precision is plenty for
    // any monitor pixel dimension we'll ever see.
    let probe = Settings::load().unwrap_or_default();
    #[allow(clippy::cast_precision_loss)]
    let initial_size =
        Size::new(probe.window.width as f32, probe.window.height as f32);

    iced::application(app::App::title, app::App::update, app::App::view)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window_size(initial_size)
        .run_with(app::App::new)
}
