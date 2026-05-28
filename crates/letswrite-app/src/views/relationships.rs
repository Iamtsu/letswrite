// Canvas-rendering view: same exemptions as `timeline.rs`.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::suboptimal_flops,
    clippy::needless_pass_by_value
)]

//! Relationships graph.
//!
//! Renders entities as nodes on a circle, with edges drawn between them
//! for every row in the `relationships` table. Clicking a node lists the
//! relationships it participates in (and lets the user open the entity's
//! document). A proper force-directed layout is deferred; circular is
//! deterministic, easy to scan, and good enough until graphs get dense.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::path::PathBuf;

use iced::mouse;
use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Stroke};
use iced::widget::{button, column, container, row, scrollable, text, Canvas};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use letswrite_core::Project;
use rusqlite::params;

const NODE_RADIUS: f32 = 8.0;
const CANVAS_HEIGHT: f32 = 480.0;

#[derive(Debug, Clone)]
pub(crate) struct NodeData {
    pub entity_id: i64,
    pub name: String,
    pub kind: String,
    pub rel_path: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct EdgeData {
    pub from_entity_id: i64,
    pub to_entity_id: i64,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    NodeSelected(i64),
    OpenSelected,
    ClearSelection,
}

#[derive(Debug, Default)]
pub(crate) struct ViewReaction {
    pub open_document: Option<PathBuf>,
}

pub(crate) struct RelationshipsView {
    nodes: Vec<NodeData>,
    edges: Vec<EdgeData>,
    /// `entity_id -> index into self.nodes`. Built when nodes refresh.
    by_id: HashMap<i64, usize>,
    selected: Option<i64>,
    canvas_cache: Cache,
}

impl std::fmt::Debug for RelationshipsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelationshipsView")
            .field("nodes", &self.nodes.len())
            .field("edges", &self.edges.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl Default for RelationshipsView {
    fn default() -> Self {
        Self::new()
    }
}

impl RelationshipsView {
    pub(crate) fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            by_id: HashMap::new(),
            selected: None,
            canvas_cache: Cache::new(),
        }
    }

    pub(crate) fn refresh(&mut self, project: &Project) {
        self.nodes = load_nodes(project);
        self.edges = load_edges(project);
        self.by_id = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.entity_id, i))
            .collect();
        // Drop selection if its entity has gone away.
        if let Some(id) = self.selected {
            if !self.by_id.contains_key(&id) {
                self.selected = None;
            }
        }
        self.canvas_cache.clear();
    }

    pub(crate) fn update(
        &mut self,
        message: Message,
        project_root: Option<&std::path::Path>,
    ) -> ViewReaction {
        match message {
            Message::NodeSelected(id) => {
                self.selected = Some(id);
                self.canvas_cache.clear();
                ViewReaction::default()
            }
            Message::ClearSelection => {
                self.selected = None;
                self.canvas_cache.clear();
                ViewReaction::default()
            }
            Message::OpenSelected => {
                let Some(id) = self.selected else {
                    return ViewReaction::default();
                };
                let Some(node) = self.by_id.get(&id).and_then(|i| self.nodes.get(*i)) else {
                    return ViewReaction::default();
                };
                let path = node
                    .rel_path
                    .as_ref()
                    .zip(project_root)
                    .map(|(rel, root)| root.join(rel));
                ViewReaction { open_document: path }
            }
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        if self.nodes.is_empty() {
            return container(
                column![
                    text("No entities to graph yet.").size(13),
                    text(
                        "Once your project has characters or locations, \
                         they'll appear here. Add `relationships` rows by \
                         tagging them in prose with [[wiki-links]] — the \
                         importer derives mention edges; explicit \
                         relationships land with the relationships editor.",
                    )
                    .size(11),
                ]
                .spacing(6)
                .padding(24),
            )
            .into();
        }

        let canvas: Canvas<&Self, Message> = Canvas::new(self)
            .width(Length::Fill)
            .height(Length::Fixed(CANVAS_HEIGHT));

        let info: Element<'_, Message> = self.selected.map_or_else(
            || {
                text(
                    "Click a node to see its relationships. \
                     Entities live on a circle; edges are relationships.",
                )
                .size(11)
                .into()
            },
            |id| selection_panel(self, id),
        );

        column![
            canvas,
            container(info).padding(8),
            scrollable(edge_list(self)).height(Length::Fill).width(Length::Fill),
        ]
        .spacing(8)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
    }
}

fn selection_panel(view: &RelationshipsView, id: i64) -> Element<'_, Message> {
    let Some(node) = view.by_id.get(&id).and_then(|i| view.nodes.get(*i)) else {
        return text("(selected entity went away)").size(11).into();
    };
    let connections = view
        .edges
        .iter()
        .filter(|e| e.from_entity_id == id || e.to_entity_id == id)
        .count();
    row![
        text(format!("{} ({}) — {} connection(s)", node.name, node.kind, connections))
            .size(12),
        button(text("Open file").size(11))
            .on_press(Message::OpenSelected)
            .style(button::primary),
        button(text("Clear").size(11))
            .on_press(Message::ClearSelection)
            .style(button::secondary),
    ]
    .spacing(8)
    .into()
}

fn edge_list(view: &RelationshipsView) -> Element<'_, Message> {
    if view.edges.is_empty() {
        return text(
            "No relationships defined yet. Add rows to `relationships` \
             once a UI for editing them lands.",
        )
        .size(11)
        .into();
    }
    let mut col = column![].spacing(2).padding(4);
    for edge in &view.edges {
        let from = view
            .by_id
            .get(&edge.from_entity_id)
            .and_then(|i| view.nodes.get(*i))
            .map_or("?", |n| n.name.as_str());
        let to = view
            .by_id
            .get(&edge.to_entity_id)
            .and_then(|i| view.nodes.get(*i))
            .map_or("?", |n| n.name.as_str());
        col = col.push(text(format!("{from}  —{}→  {to}", edge.kind)).size(11));
    }
    col.into()
}

impl canvas::Program<Message> for RelationshipsView {
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
            draw_graph(frame, &self.nodes, &self.edges, self.selected);
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
        let centers = node_centers(&self.nodes, bounds.size());
        for (i, c) in centers.iter().enumerate() {
            let dx = pos.x - c.x;
            let dy = pos.y - c.y;
            if dx * dx + dy * dy <= NODE_RADIUS * NODE_RADIUS * 4.0 {
                return Some(
                    canvas::Action::publish(Message::NodeSelected(self.nodes[i].entity_id))
                        .and_capture(),
                );
            }
        }
        None
    }
}

fn node_centers(nodes: &[NodeData], bounds: Size) -> Vec<Point> {
    let cx = bounds.width / 2.0;
    let cy = bounds.height / 2.0;
    let radius = (bounds.width.min(bounds.height) / 2.0 - 60.0).max(40.0);
    let n = nodes.len() as f32;
    (0..nodes.len())
        .map(|i| {
            let angle = (i as f32 / n) * TAU - TAU / 4.0;
            Point::new(cx + radius * angle.cos(), cy + radius * angle.sin())
        })
        .collect()
}

fn draw_graph(
    frame: &mut Frame,
    nodes: &[NodeData],
    edges: &[EdgeData],
    selected: Option<i64>,
) {
    if nodes.is_empty() {
        return;
    }
    let bounds = frame.size();
    let centers = node_centers(nodes, bounds);
    let by_id: HashMap<i64, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.entity_id, i))
        .collect();

    // Edges first so nodes sit on top.
    for edge in edges {
        let (Some(&i), Some(&j)) =
            (by_id.get(&edge.from_entity_id), by_id.get(&edge.to_entity_id))
        else {
            continue;
        };
        let line = Path::line(centers[i], centers[j]);
        let highlighted = selected.is_some_and(|id| {
            edge.from_entity_id == id || edge.to_entity_id == id
        });
        let stroke = Stroke::default()
            .with_color(if highlighted {
                Color::from_rgba(0.95, 0.7, 0.2, 0.95)
            } else {
                Color::from_rgba(0.55, 0.55, 0.55, 0.4)
            })
            .with_width(if highlighted { 2.0 } else { 1.0 });
        frame.stroke(&line, stroke);
    }

    for (i, node) in nodes.iter().enumerate() {
        let is_selected = Some(node.entity_id) == selected;
        let radius = if is_selected { NODE_RADIUS * 1.6 } else { NODE_RADIUS };
        let circle = Path::circle(centers[i], radius);
        frame.fill(&circle, color_for_kind(&node.kind));
        if is_selected {
            frame.stroke(
                &circle,
                Stroke::default()
                    .with_color(Color::from_rgb(1.0, 0.95, 0.2))
                    .with_width(2.0),
            );
        }
    }
}

fn color_for_kind(kind: &str) -> Color {
    // Distinct palette per entity-kind so character vs location vs item
    // shows immediately.
    match kind {
        "character" => Color::from_rgb(0.34, 0.70, 0.91),    // sky blue
        "location" => Color::from_rgb(0.00, 0.62, 0.45),     // bluish green
        "faction" => Color::from_rgb(0.80, 0.47, 0.65),      // reddish purple
        "item" => Color::from_rgb(0.90, 0.62, 0.00),         // orange
        _ => Color::from_rgb(0.60, 0.60, 0.60),
    }
}

fn load_nodes(project: &Project) -> Vec<NodeData> {
    let conn = project.database().conn();
    let mut stmt = match conn.prepare(
        "SELECT e.id, e.name, e.kind,
                (SELECT rel_path FROM documents WHERE id = e.document_id)
           FROM entities e
          WHERE e.project_id = ?1
          ORDER BY e.kind, e.name",
    ) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "relationships node query failed");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(params![project.id()], |r| {
        Ok(NodeData {
            entity_id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            rel_path: r.get(3)?,
        })
    });
    rows.map(|it| it.flatten().collect()).unwrap_or_default()
}

fn load_edges(project: &Project) -> Vec<EdgeData> {
    let conn = project.database().conn();
    let mut stmt = match conn.prepare(
        "SELECT r.from_entity_id, r.to_entity_id, r.kind
           FROM relationships r
           JOIN entities e ON e.id = r.from_entity_id
          WHERE e.project_id = ?1",
    ) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "relationships edge query failed");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(params![project.id()], |r| {
        Ok(EdgeData {
            from_entity_id: r.get(0)?,
            to_entity_id: r.get(1)?,
            kind: r.get(2)?,
        })
    });
    rows.map(|it| it.flatten().collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: i64, name: &str, kind: &str) -> NodeData {
        NodeData {
            entity_id: id,
            name: name.into(),
            kind: kind.into(),
            rel_path: None,
        }
    }

    #[test]
    fn node_centers_distribute_evenly_on_a_circle() {
        let nodes = (0..4).map(|i| node(i, "n", "character")).collect::<Vec<_>>();
        let centers = node_centers(&nodes, Size::new(400.0, 400.0));
        assert_eq!(centers.len(), 4);
        // The 4 centers should land on 4 of the 4 cardinal directions
        // from the canvas center.
        let cx = 200.0;
        let cy = 200.0;
        let mut offsets: Vec<(f32, f32)> = centers
            .iter()
            .map(|p| ((p.x - cx).round(), (p.y - cy).round()))
            .collect();
        offsets.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        // Two with x ≈ 0 and two with y ≈ 0 (the 4 cardinal points).
        let zero = offsets.iter().filter(|(x, _)| x.abs() < 1.0).count();
        let zero_y = offsets.iter().filter(|(_, y)| y.abs() < 1.0).count();
        assert!(zero == 2 && zero_y == 2, "got offsets: {offsets:?}");
    }

    #[test]
    fn color_for_kind_is_stable_per_kind() {
        assert_eq!(color_for_kind("character"), color_for_kind("character"));
        assert_ne!(color_for_kind("character"), color_for_kind("location"));
    }
}
