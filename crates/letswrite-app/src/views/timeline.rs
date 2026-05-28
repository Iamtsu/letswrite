// Canvas-rendering view: per-pixel coordinate math with usize→f32 casts is
// the norm here. Suppress the lints that fight that idiom.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::suboptimal_flops,
    clippy::needless_pass_by_value
)]

//! Horizontal plot/timeline view.
//!
//! Renders every scene as a colored bar on a horizontal track, color-coded
//! by POV character (toggle: color by location). Clicking a bar opens the
//! scene's document in the editor. Below the track, a per-character row
//! shows which scenes that character appears in (presence lanes).

use std::collections::HashMap;
use std::path::PathBuf;

use iced::mouse;
use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Stroke};
use iced::widget::{button, column, container, row, scrollable, text, Canvas};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use letswrite_core::Project;
use rusqlite::params;

const TRACK_HEIGHT: f32 = 36.0;
const LANE_HEIGHT: f32 = 18.0;
const LANE_GAP: f32 = 2.0;
const HEADER_HEIGHT: f32 = 24.0;
const PADDING: f32 = 12.0;

#[derive(Debug, Clone)]
pub(crate) struct SceneBar {
    pub scene_id: i64,
    pub document_rel_path: String,
    pub synopsis: String,
    pub pov: Option<String>,
    pub location: Option<String>,
    /// Names of every character whose mention appears in this scene's body.
    pub characters_present: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorBy {
    Pov,
    Location,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    SetColorBy(ColorBy),
    OpenScene(i64),
}

#[derive(Debug, Default)]
pub(crate) struct ViewReaction {
    pub open_document: Option<PathBuf>,
}

pub(crate) struct TimelineView {
    bars: Vec<SceneBar>,
    /// Stable ordering of distinct POV character names, for the lane rows.
    characters: Vec<String>,
    color_by: ColorBy,
    canvas_cache: Cache,
}

impl std::fmt::Debug for TimelineView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimelineView")
            .field("bars", &self.bars.len())
            .field("characters", &self.characters.len())
            .field("color_by", &self.color_by)
            .finish_non_exhaustive()
    }
}

impl Default for TimelineView {
    fn default() -> Self {
        Self::new()
    }
}

impl TimelineView {
    pub(crate) fn new() -> Self {
        Self {
            bars: Vec::new(),
            characters: Vec::new(),
            color_by: ColorBy::Pov,
            canvas_cache: Cache::new(),
        }
    }

    pub(crate) fn refresh(&mut self, project: &Project) {
        self.bars = load_scene_bars(project);
        self.characters = derive_character_lanes(&self.bars);
        self.canvas_cache.clear();
    }

    pub(crate) fn update(
        &mut self,
        message: Message,
        project_root: Option<&std::path::Path>,
    ) -> ViewReaction {
        match message {
            Message::SetColorBy(c) => {
                self.color_by = c;
                self.canvas_cache.clear();
                ViewReaction::default()
            }
            Message::OpenScene(scene_id) => {
                let Some(bar) = self.bars.iter().find(|b| b.scene_id == scene_id) else {
                    return ViewReaction::default();
                };
                let path = project_root.map(|r| r.join(&bar.document_rel_path));
                ViewReaction { open_document: path }
            }
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        if self.bars.is_empty() {
            return container(
                column![
                    text("No scenes yet.").size(13),
                    text(
                        "Once chapters split into scenes (via `## Beat N: \
                         Title`) and the importer runs, you'll see them \
                         here on a horizontal track.",
                    )
                    .size(11),
                ]
                .spacing(6)
                .padding(24),
            )
            .into();
        }
        let header = row![
            text("Color by:").size(12),
            button(text("POV").size(11))
                .on_press(Message::SetColorBy(ColorBy::Pov))
                .style(if self.color_by == ColorBy::Pov {
                    button::primary
                } else {
                    button::secondary
                }),
            button(text("Location").size(11))
                .on_press(Message::SetColorBy(ColorBy::Location))
                .style(if self.color_by == ColorBy::Location {
                    button::primary
                } else {
                    button::secondary
                }),
        ]
        .spacing(6)
        .padding(8);

        let height = HEADER_HEIGHT
            + TRACK_HEIGHT
            + LANE_GAP
            + (self.characters.len() as f32 * (LANE_HEIGHT + LANE_GAP))
            + PADDING * 2.0;

        let canvas: Canvas<&Self, Message> =
            Canvas::new(self).width(Length::Fill).height(Length::Fixed(height));

        let scene_list = scrollable(scene_list(&self.bars))
            .height(Length::Fill)
            .width(Length::Fill);

        column![header, canvas, scene_list]
            .spacing(8)
            .height(Length::Fill)
            .width(Length::Fill)
            .into()
    }
}

fn scene_list(bars: &[SceneBar]) -> Element<'_, Message> {
    let mut col = column![].spacing(4).padding(8);
    for (i, b) in bars.iter().enumerate() {
        let id = b.scene_id;
        let pov_str = b.pov.clone().unwrap_or_else(|| "?".into());
        let loc_str = b.location.clone().unwrap_or_default();
        let line = format!(
            "{:>3}. {}  —  POV {}{}",
            i + 1,
            b.synopsis,
            pov_str,
            if loc_str.is_empty() {
                String::new()
            } else {
                format!(" @ {loc_str}")
            },
        );
        col = col.push(
            button(text(line).size(11))
                .on_press(Message::OpenScene(id))
                .style(button::text)
                .width(Length::Fill),
        );
    }
    col.into()
}

impl canvas::Program<Message> for TimelineView {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geometry = self.canvas_cache.draw(renderer, bounds.size(), |frame| {
            draw_timeline(frame, &self.bars, &self.characters, self.color_by);
        });
        vec![geometry]
    }

    fn update(
        &self,
        _state: &mut Self::State,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event
        else {
            return None;
        };
        let pos = cursor.position_in(bounds)?;
        if let Some(scene_id) = hit_test(pos, bounds.size(), &self.bars) {
            return Some(
                canvas::Action::publish(Message::OpenScene(scene_id)).and_capture(),
            );
        }
        None
    }
}

fn draw_timeline(
    frame: &mut Frame,
    bars: &[SceneBar],
    characters: &[String],
    color_by: ColorBy,
) {
    if bars.is_empty() {
        return;
    }
    let n = bars.len() as f32;
    let bounds = frame.size();
    let track_width = bounds.width - PADDING * 2.0;
    let bar_width = (track_width / n).max(2.0);
    let track_y = PADDING + HEADER_HEIGHT;

    // Background track line.
    let line = Path::line(
        Point::new(PADDING, track_y + TRACK_HEIGHT / 2.0),
        Point::new(PADDING + track_width, track_y + TRACK_HEIGHT / 2.0),
    );
    frame.stroke(
        &line,
        Stroke::default()
            .with_color(Color::from_rgba(0.5, 0.5, 0.5, 0.4))
            .with_width(1.0),
    );

    // Scene bars on the main track.
    for (i, bar) in bars.iter().enumerate() {
        let x = PADDING + (i as f32) * bar_width;
        let color = color_for_bar(bar, color_by);
        let rect = Path::rectangle(
            Point::new(x + 1.0, track_y + 4.0),
            Size::new((bar_width - 2.0).max(2.0), TRACK_HEIGHT - 8.0),
        );
        frame.fill(&rect, color);
    }

    // Character presence lanes.
    for (lane_idx, name) in characters.iter().enumerate() {
        let lane_y = track_y
            + TRACK_HEIGHT
            + LANE_GAP
            + (lane_idx as f32) * (LANE_HEIGHT + LANE_GAP);
        let baseline = Path::line(
            Point::new(PADDING, lane_y + LANE_HEIGHT / 2.0),
            Point::new(PADDING + track_width, lane_y + LANE_HEIGHT / 2.0),
        );
        frame.stroke(
            &baseline,
            Stroke::default()
                .with_color(Color::from_rgba(0.5, 0.5, 0.5, 0.15))
                .with_width(1.0),
        );
        for (i, bar) in bars.iter().enumerate() {
            if !bar.characters_present.iter().any(|c| c == name) {
                continue;
            }
            let x = PADDING + (i as f32) * bar_width;
            let rect = Path::rectangle(
                Point::new(x + 1.0, lane_y + 4.0),
                Size::new((bar_width - 2.0).max(2.0), LANE_HEIGHT - 8.0),
            );
            frame.fill(&rect, color_for_name(name));
        }
    }
}

fn hit_test(pos: Point, bounds: Size, bars: &[SceneBar]) -> Option<i64> {
    if bars.is_empty() {
        return None;
    }
    let track_y = PADDING + HEADER_HEIGHT;
    if pos.y < track_y || pos.y > track_y + TRACK_HEIGHT {
        return None;
    }
    let track_width = bounds.width - PADDING * 2.0;
    let n = bars.len() as f32;
    let bar_width = (track_width / n).max(2.0);
    let rel_x = pos.x - PADDING;
    if rel_x < 0.0 || rel_x > track_width {
        return None;
    }
    let idx = (rel_x / bar_width).floor() as usize;
    bars.get(idx).map(|b| b.scene_id)
}

fn load_scene_bars(project: &Project) -> Vec<SceneBar> {
    let conn = project.database().conn();
    let mut stmt = match conn.prepare(
        "SELECT s.id, d.rel_path, s.synopsis,
                COALESCE(pov.name, ''), COALESCE(loc.name, '')
           FROM scenes s
           JOIN documents d ON d.id = s.document_id
           LEFT JOIN entities pov ON pov.id = s.pov_entity_id
           LEFT JOIN entities loc ON loc.id = s.location_entity_id
          WHERE d.project_id = ?1
          ORDER BY d.rel_path, s.order_index",
    ) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "timeline scene query failed");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(params![project.id()], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(%err, "timeline scene iteration failed");
            return Vec::new();
        }
    };
    let mut bars: Vec<SceneBar> = rows
        .flatten()
        .map(|(scene_id, document_rel_path, synopsis, pov, loc)| SceneBar {
            scene_id,
            document_rel_path,
            synopsis,
            pov: if pov.is_empty() { None } else { Some(pov) },
            location: if loc.is_empty() { None } else { Some(loc) },
            characters_present: Vec::new(),
        })
        .collect();
    // Populate characters_present per bar via a second query.
    let document_ids: Vec<String> = bars
        .iter()
        .map(|b| b.document_rel_path.clone())
        .collect();
    let _ = document_ids;
    let mut by_doc: HashMap<String, Vec<String>> = HashMap::new();
    let mut stmt = match conn.prepare(
        "SELECT d.rel_path, e.name
           FROM entity_mentions em
           JOIN entities e ON e.id = em.entity_id
           JOIN documents d ON d.id = em.document_id
          WHERE d.project_id = ?1 AND e.kind = 'character'",
    ) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "timeline character query failed");
            return bars;
        }
    };
    let rows = stmt.query_map(params![project.id()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    });
    if let Ok(rows) = rows {
        for (rel, name) in rows.flatten() {
            let entry = by_doc.entry(rel).or_default();
            if !entry.contains(&name) {
                entry.push(name);
            }
        }
    }
    for bar in &mut bars {
        if let Some(names) = by_doc.get(&bar.document_rel_path) {
            bar.characters_present = names.clone();
        }
    }
    bars
}

fn derive_character_lanes(bars: &[SceneBar]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for bar in bars {
        for name in &bar.characters_present {
            if !seen.contains(name) {
                seen.push(name.clone());
            }
        }
    }
    seen
}

fn color_for_bar(bar: &SceneBar, color_by: ColorBy) -> Color {
    let key = match color_by {
        ColorBy::Pov => bar.pov.as_deref(),
        ColorBy::Location => bar.location.as_deref(),
    };
    key.map_or_else(|| Color::from_rgba(0.6, 0.6, 0.6, 0.6), color_for_name)
}

/// Deterministic, color-blind-safe palette (Okabe-Ito) keyed by a stable
/// hash of the entity name. Same name = same color across runs.
fn color_for_name(name: &str) -> Color {
    const PALETTE: &[(u8, u8, u8)] = &[
        (0xE6, 0x9F, 0x00), // orange
        (0x56, 0xB4, 0xE9), // sky blue
        (0x00, 0x9E, 0x73), // bluish green
        (0xCC, 0x79, 0xA7), // reddish purple
        (0xF0, 0xE4, 0x42), // yellow
        (0x00, 0x72, 0xB2), // blue
        (0xD5, 0x5E, 0x00), // vermillion
    ];
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    name.trim().to_ascii_lowercase().hash(&mut h);
    let idx = (h.finish() as usize) % PALETTE.len();
    let (r, g, b) = PALETTE[idx];
    Color::from_rgb(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_for_name_is_deterministic() {
        let a = color_for_name("Evan Calder");
        let b = color_for_name("evan calder");
        let c = color_for_name(" Evan Calder ");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn color_distinct_for_distinct_names_in_palette() {
        // Not guaranteed for *every* pair (hash collisions exist) but the
        // dogfood novel's characters should mostly land on different
        // palette slots.
        let evan = color_for_name("Evan Calder");
        let marcus = color_for_name("Marcus Webb");
        assert_ne!(evan, marcus);
    }

    #[test]
    fn derive_lanes_dedupes_and_preserves_order() {
        let bars = vec![
            SceneBar {
                scene_id: 1,
                document_rel_path: "x".into(),
                synopsis: "s".into(),
                pov: None,
                location: None,
                characters_present: vec!["Evan".into(), "Mara".into()],
            },
            SceneBar {
                scene_id: 2,
                document_rel_path: "y".into(),
                synopsis: "s2".into(),
                pov: None,
                location: None,
                characters_present: vec!["Mara".into(), "Aletheia".into()],
            },
        ];
        let lanes = derive_character_lanes(&bars);
        assert_eq!(lanes, vec!["Evan", "Mara", "Aletheia"]);
    }
}
