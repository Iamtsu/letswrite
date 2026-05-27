// Canvas-rendering helper: same lint exemptions as the timeline view.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::suboptimal_flops
)]

//! Character minimap — a tiny constellation of every character in the
//! project, with the ones present in the current scene highlighted.
//!
//! Lives at the top of the assistant column (in [`crate::assistant`]) so
//! it updates whenever the cursor moves. Distinct from the
//! [`crate::views::relationships`] graph: the minimap is a glanceable
//! header strip, not a full view.

use std::f32::consts::TAU;

use iced::mouse;
use iced::widget::canvas::{self, Cache, Frame, Geometry, Path};
use iced::widget::Canvas;
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

const HEIGHT: f32 = 80.0;
const RADIUS: f32 = 4.5;
const HIGHLIGHT_RADIUS: f32 = 7.0;

/// One character node on the minimap.
#[derive(Debug, Clone)]
pub(crate) struct Star {
    /// `true` if this character is one of the entities present in the
    /// current scene. Highlighted bigger and brighter.
    pub present: bool,
}

#[derive(Default)]
pub(crate) struct Minimap {
    stars: Vec<Star>,
    cache: Cache,
}

impl std::fmt::Debug for Minimap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Minimap")
            .field("stars", &self.stars.len())
            .finish_non_exhaustive()
    }
}

impl Minimap {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Refresh the constellation. The full character list comes from the
    /// project; `present_names` are the ones to highlight.
    pub(crate) fn set_state(
        &mut self,
        all_characters: &[String],
        present_names: &[String],
    ) {
        let lowered_present: Vec<String> = present_names
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .collect();
        self.stars = all_characters
            .iter()
            .map(|name| Star {
                present: lowered_present
                    .contains(&name.trim().to_ascii_lowercase()),
            })
            .collect();
        self.cache.clear();
    }

    pub(crate) fn view<Message: 'static>(&self) -> Element<'_, Message> {
        Canvas::new(MinimapProgram { stars: &self.stars, cache: &self.cache })
            .width(Length::Fill)
            .height(Length::Fixed(HEIGHT))
            .into()
    }
}

struct MinimapProgram<'a> {
    stars: &'a [Star],
    cache: &'a Cache,
}

impl<Message> canvas::Program<Message> for MinimapProgram<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let g = self.cache.draw(renderer, bounds.size(), |frame| {
            draw_minimap(frame, self.stars);
        });
        vec![g]
    }
}

fn draw_minimap(frame: &mut Frame, stars: &[Star]) {
    if stars.is_empty() {
        return;
    }
    let bounds = frame.size();
    let positions = star_positions(stars.len(), bounds);
    for (i, star) in stars.iter().enumerate() {
        let pos = positions[i];
        let (color, radius) = if star.present {
            (Color::from_rgb(0.95, 0.78, 0.20), HIGHLIGHT_RADIUS)
        } else {
            (Color::from_rgba(0.55, 0.55, 0.55, 0.55), RADIUS)
        };
        let circle = Path::circle(pos, radius);
        frame.fill(&circle, color);
    }
}

/// Position N stars in a compact arc near the top of the canvas. We use a
/// shallow arc (top half of a wide ellipse) so even tens of characters
/// fit horizontally without crowding the vertical space.
fn star_positions(n: usize, bounds: Size) -> Vec<Point> {
    if n == 0 {
        return Vec::new();
    }
    let pad = 12.0;
    let cx = bounds.width / 2.0;
    let cy = bounds.height * 0.6;
    let rx = (bounds.width / 2.0) - pad;
    let ry = (bounds.height / 2.0) - pad;
    if n == 1 {
        return vec![Point::new(cx, cy)];
    }
    (0..n)
        .map(|i| {
            // Spread across the lower half of a TAU/3 arc so the arc looks
            // like a slight bowl, not a full ellipse.
            let span = TAU * 0.4;
            let t = i as f32 / (n - 1) as f32;
            let angle = -span / 2.0 + t * span - TAU / 4.0;
            Point::new(cx + rx * angle.sin(), cy + ry * angle.cos() * 0.5)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_state_marks_present_case_insensitively() {
        let mut m = Minimap::new();
        m.set_state(
            &[
                "Evan Calder".to_owned(),
                "Aletheia".to_owned(),
                "Marcus Webb".to_owned(),
            ],
            &["evan calder".to_owned(), " Aletheia ".to_owned()],
        );
        assert_eq!(m.stars.len(), 3);
        assert!(m.stars[0].present); // Evan
        assert!(m.stars[1].present); // Aletheia
        assert!(!m.stars[2].present); // Marcus
    }

    #[test]
    fn star_positions_returns_one_per_star() {
        assert_eq!(star_positions(0, Size::new(200.0, 80.0)), Vec::new());
        assert_eq!(star_positions(1, Size::new(200.0, 80.0)).len(), 1);
        assert_eq!(star_positions(10, Size::new(200.0, 80.0)).len(), 10);
    }
}
