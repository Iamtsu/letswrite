//! Scene cards / corkboard view.
//!
//! Renders one card per scene from the `scenes` table, grouped by chapter
//! document. Each card shows the scene's synopsis (heading text), status,
//! POV character, location, and a snippet from the body window.
//!
//! Reordering is move-up / move-down per card (true drag-and-drop in Iced
//! is non-trivial and not worth the complexity for v1). Reordering
//! rewrites both the `scenes.order_index` column AND the chapter's
//! `## Beat N: Title` lines on disk so the textual order matches.

use std::path::PathBuf;

use iced::widget::{button, column, container, row, rule, scrollable, text};
use iced::{Element, Length};

use letswrite_core::{Document, Project};
use rusqlite::params;

#[derive(Debug, Clone)]
pub(crate) struct SceneCard {
    pub scene_id: i64,
    pub document_id: i64,
    pub document_title: String,
    pub document_rel_path: String,
    pub order_index: f64,
    pub synopsis: String,
    pub status: String,
    pub pov: String,
    pub location: String,
    /// Short snippet from the scene body (first ~200 chars after the
    /// heading).
    pub snippet: String,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    MoveUp(i64),
    MoveDown(i64),
    /// Open the document at the given scene's start in the editor.
    OpenScene(i64),
}

#[derive(Debug, Default)]
pub(crate) struct ViewReaction {
    pub fs_changed: bool,
    pub open_document: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub(crate) struct CorkboardView {
    cards: Vec<SceneCard>,
}

impl CorkboardView {
    pub(crate) const fn new() -> Self {
        Self { cards: Vec::new() }
    }

    pub(crate) fn refresh(&mut self, project: &Project, project_root: &std::path::Path) {
        self.cards = load_scene_cards(project, project_root);
    }

    // Iced's update idiom takes the message by value; clippy disagrees.
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn update(
        &mut self,
        message: Message,
        project: Option<&mut Project>,
        project_root: Option<&std::path::Path>,
    ) -> ViewReaction {
        match message {
            Message::MoveUp(scene_id) => self.move_scene(scene_id, -1, project, project_root),
            Message::MoveDown(scene_id) => self.move_scene(scene_id, 1, project, project_root),
            Message::OpenScene(scene_id) => {
                let Some(card) = self.cards.iter().find(|c| c.scene_id == scene_id) else {
                    return ViewReaction::default();
                };
                let path = project_root
                    .map(|root| root.join(&card.document_rel_path));
                ViewReaction {
                    open_document: path,
                    ..Default::default()
                }
            }
        }
    }

    fn move_scene(
        &mut self,
        scene_id: i64,
        direction: i32,
        project: Option<&mut Project>,
        project_root: Option<&std::path::Path>,
    ) -> ViewReaction {
        let Some(project) = project else { return ViewReaction::default() };
        let Some(root) = project_root else { return ViewReaction::default() };

        // Find this card and its neighbour within the same document.
        let Some(idx) = self.cards.iter().position(|c| c.scene_id == scene_id) else {
            return ViewReaction::default();
        };
        let target_idx = if direction < 0 {
            if idx == 0 { return ViewReaction::default(); }
            idx - 1
        } else {
            if idx + 1 >= self.cards.len() { return ViewReaction::default(); }
            idx + 1
        };

        // Only swap within the same chapter document.
        if self.cards[idx].document_id != self.cards[target_idx].document_id {
            return ViewReaction::default();
        }

        // Swap the order_index values in the DB.
        let a = self.cards[idx].clone();
        let b = self.cards[target_idx].clone();
        if let Err(err) = swap_order_indices(project, a.scene_id, b.order_index, b.scene_id, a.order_index) {
            tracing::warn!(%err, "scene reorder DB update failed");
            return ViewReaction::default();
        }

        // Rewrite the chapter file so the `## Beat N:` lines reflect the
        // new order. We re-derive scene order from the DB after the swap.
        let doc_id = a.document_id;
        let rel_path = a.document_rel_path;
        if let Err(err) = rewrite_chapter_for(project, root, doc_id, &rel_path) {
            tracing::warn!(%err, "chapter rewrite after reorder failed");
        }

        self.refresh(project, root);
        ViewReaction { fs_changed: true, ..Default::default() }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        if self.cards.is_empty() {
            return container(
                column![
                    text("No scenes yet.").size(13),
                    text(
                        "Chapters split into scenes on `## Beat N: Title` \
                         headings. Add one to a chapter file and re-index \
                         to see cards here.",
                    )
                    .size(11),
                ]
                .spacing(6)
                .padding(24),
            )
            .into();
        }

        let mut col = column![].spacing(16).padding(16);
        let mut current_doc: Option<i64> = None;
        for card in &self.cards {
            if Some(card.document_id) != current_doc {
                if current_doc.is_some() {
                    col = col.push(rule::horizontal(1.0));
                }
                col = col.push(text(card.document_title.clone()).size(14));
                current_doc = Some(card.document_id);
            }
            col = col.push(render_card(card));
        }
        scrollable(col).height(Length::Fill).width(Length::Fill).into()
    }
}

fn render_card(card: &SceneCard) -> Element<'_, Message> {
    use std::fmt::Write as _;
    let id = card.scene_id;
    let header = row![
        text(card.synopsis.clone()).size(13).width(Length::Fill),
        button(text("↑").size(11))
            .on_press(Message::MoveUp(id))
            .style(button::secondary),
        button(text("↓").size(11))
            .on_press(Message::MoveDown(id))
            .style(button::secondary),
        button(text("Open").size(11))
            .on_press(Message::OpenScene(id))
            .style(button::primary),
    ]
    .spacing(4);

    let mut meta = String::new();
    if !card.status.is_empty() {
        let _ = write!(meta, "[{}]", card.status);
    }
    if !card.pov.is_empty() {
        if !meta.is_empty() { meta.push(' '); }
        let _ = write!(meta, "POV: {}", card.pov);
    }
    if !card.location.is_empty() {
        if !meta.is_empty() { meta.push(' '); }
        let _ = write!(meta, "@ {}", card.location);
    }

    let mut col = column![header].spacing(4);
    if !meta.is_empty() {
        col = col.push(text(meta).size(10));
    }
    if !card.snippet.is_empty() {
        col = col.push(text(card.snippet.clone()).size(11));
    }

    container(col).padding(10).into()
}

fn load_scene_cards(project: &Project, project_root: &std::path::Path) -> Vec<SceneCard> {
    let conn = project.database().conn();
    let mut stmt = match conn.prepare(
        "SELECT s.id, d.id, d.title, d.rel_path, s.order_index, s.synopsis, s.status,
                COALESCE(pov.name, ''), COALESCE(loc.name, ''),
                s.start_offset, s.end_offset
           FROM scenes s
           JOIN documents d ON d.id = s.document_id
           LEFT JOIN entities pov ON pov.id = s.pov_entity_id
           LEFT JOIN entities loc ON loc.id = s.location_entity_id
          WHERE d.project_id = ?1
          ORDER BY d.rel_path, s.order_index",
    ) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "scene card query failed");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(params![project.id()], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, f64>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, i64>(9)?,
            r.get::<_, i64>(10)?,
        ))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(%err, "scene card iteration failed");
            return Vec::new();
        }
    };
    rows.flatten()
        .map(|(scene_id, document_id, title, rel_path, order_index, synopsis, status, pov, location, start, end)| {
            let snippet = body_snippet(project_root, &rel_path, start, end);
            SceneCard {
                scene_id,
                document_id,
                document_title: title,
                document_rel_path: rel_path,
                order_index,
                synopsis,
                status,
                pov,
                location,
                snippet,
            }
        })
        .collect()
}

fn body_snippet(project_root: &std::path::Path, rel_path: &str, start: i64, end: i64) -> String {
    let abs = project_root.join(rel_path);
    let Ok(doc) = Document::load(project_root, &abs) else {
        return String::new();
    };
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let s = (start.max(0) as usize).min(doc.body.len());
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let e = (end.max(0) as usize).min(doc.body.len());
    let raw = doc.body.get(s..e).unwrap_or("");
    // Strip the heading line and trim to ~200 chars.
    let after_heading = raw.split_once('\n').map_or(raw, |(_, rest)| rest);
    let mut snippet: String = after_heading.chars().take(200).collect();
    if after_heading.chars().count() > 200 {
        snippet.push('…');
    }
    snippet.replace('\n', " ").trim().to_owned()
}

#[allow(clippy::similar_names)] // a/b naming is the clearest here.
fn swap_order_indices(
    project: &mut Project,
    scene_a: i64,
    new_a_order: f64,
    scene_b: i64,
    new_b_order: f64,
) -> letswrite_core::Result<()> {
    let conn = project.database_mut().conn_mut();
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE scenes SET order_index = ?1 WHERE id = ?2",
        params![new_a_order, scene_a],
    )?;
    tx.execute(
        "UPDATE scenes SET order_index = ?1 WHERE id = ?2",
        params![new_b_order, scene_b],
    )?;
    tx.commit()?;
    Ok(())
}

/// Rewrite a chapter's on-disk Markdown so its `## Beat N:` lines appear
/// in the same order as `scenes.order_index`. We preserve each beat's
/// body verbatim and renumber the `N` based on the new order.
fn rewrite_chapter_for(
    project: &Project,
    project_root: &std::path::Path,
    document_id: i64,
    rel_path: &str,
) -> letswrite_core::Result<()> {
    use std::fmt::Write as _;
    let abs = project_root.join(rel_path);
    let doc = Document::load(project_root, &abs)?;
    let beats = parse_beats(&doc.body);
    if beats.is_empty() {
        return Ok(());
    }
    // Pull the desired order from the DB by matching on synopsis (the
    // heading text used by the importer). When multiple beats share a
    // title we keep their pre-existing relative order.
    let conn = project.database().conn();
    let mut stmt = conn.prepare(
        "SELECT synopsis FROM scenes WHERE document_id = ?1 ORDER BY order_index",
    )?;
    let titles: Vec<String> = stmt
        .query_map(params![document_id], |r| r.get::<_, String>(0))?
        .flatten()
        .collect();

    let mut remaining: Vec<Beat> = beats;
    let mut ordered: Vec<Beat> = Vec::with_capacity(titles.len());
    for title in &titles {
        if let Some(pos) = remaining.iter().position(|b| b.title == *title) {
            ordered.push(remaining.remove(pos));
        }
    }
    // Append anything the DB didn't know about (preserves edits made
    // outside letswrite).
    ordered.extend(remaining);

    let preamble_end = ordered.first().map_or(doc.body.len(), |b| b.start_offset);
    let preamble = &doc.body[..preamble_end];

    let mut new_body = String::with_capacity(doc.body.len());
    new_body.push_str(preamble);
    for (i, beat) in ordered.iter().enumerate() {
        let n = i + 1;
        let _ = writeln!(new_body, "## Beat {n}: {}", beat.title);
        new_body.push_str(&beat.body);
        if !beat.body.ends_with('\n') {
            new_body.push('\n');
        }
    }

    let new_doc = Document {
        rel_path: doc.rel_path,
        kind: doc.kind,
        title: doc.title,
        frontmatter: doc.frontmatter,
        body: new_body,
    };
    new_doc.save(project_root)
}

#[derive(Debug, Clone)]
struct Beat {
    title: String,
    start_offset: usize,
    body: String,
}

fn parse_beats(body: &str) -> Vec<Beat> {
    // Mirrors letswrite_import::scenes::parse_beats, but we also keep
    // each beat's body text for round-tripping.
    let mut hits: Vec<(usize, String)> = Vec::new();
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if let Some(title) = parse_beat_heading(trimmed) {
            hits.push((offset, title.to_owned()));
        }
        offset += line.len();
    }
    let mut out = Vec::with_capacity(hits.len());
    for i in 0..hits.len() {
        let (start, title) = (hits[i].0, hits[i].1.clone());
        let end = if i + 1 < hits.len() { hits[i + 1].0 } else { body.len() };
        // The body for a beat = text after the heading line up to the next beat.
        let heading_end = body[start..end]
            .find('\n')
            .map_or(end, |p| start + p + 1);
        let beat_body = body[heading_end..end].to_owned();
        out.push(Beat { title, start_offset: start, body: beat_body });
    }
    out
}

fn parse_beat_heading(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("## ")?;
    let lower = rest.to_ascii_lowercase();
    if !lower.starts_with("beat") {
        return None;
    }
    let after = rest.strip_prefix("beat").or_else(|| rest.strip_prefix("Beat"))?;
    let after = after.trim_start();
    let colon = after.find(':')?;
    let (num, rest) = after.split_at(colon);
    if !num.chars().all(|c| c.is_ascii_digit() || c.is_whitespace()) {
        return None;
    }
    let title = rest.trim_start_matches(':').trim();
    if title.is_empty() { None } else { Some(title) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_beats_preserves_body() {
        let body = "preamble\n\n## Beat 1: A\n\none body\n\n## Beat 2: B\n\ntwo body\n";
        let beats = parse_beats(body);
        assert_eq!(beats.len(), 2);
        assert_eq!(beats[0].title, "A");
        assert!(beats[0].body.contains("one body"));
        assert_eq!(beats[1].title, "B");
        assert!(beats[1].body.contains("two body"));
    }

    #[test]
    fn rewrite_renumbers_beats() {
        use std::fmt::Write as _;
        let body = "## Beat 1: First\n\nfirst body\n## Beat 2: Second\n\nsecond body\n";
        let beats = parse_beats(body);
        assert_eq!(beats.len(), 2);
        // Pretend "Second" should come first (as if the DB reordered).
        let new_order = [beats[1].clone(), beats[0].clone()];
        let preamble_end = beats[0].start_offset;
        let preamble = &body[..preamble_end];
        let mut new_body = String::new();
        new_body.push_str(preamble);
        for (i, b) in new_order.iter().enumerate() {
            let _ = writeln!(new_body, "## Beat {}: {}", i + 1, b.title);
            new_body.push_str(&b.body);
        }
        assert!(new_body.contains("## Beat 1: Second"));
        assert!(new_body.contains("## Beat 2: First"));
        assert!(new_body.find("first body").unwrap() > new_body.find("second body").unwrap());
    }
}
