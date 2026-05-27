//! Research / worldbuilding notes view.
//!
//! Browses every entity that lives under `Research/` — i.e. factions,
//! items, concepts, lore. Same structural shape as the character and
//! location views, with a free-form notes body and a flexible `kind`
//! frontmatter field that lets writers organize their notes however
//! they want.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::widget::{button, column, container, horizontal_rule, row, scrollable, text, text_input};
use iced::{Element, Length, Task};

use letswrite_core::{Document, DocumentKind, Project};
use rusqlite::params;
use serde_yaml::Value as YamlValue;

const SAVE_IDLE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub(crate) struct ResearchCard {
    pub entity_id: i64,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ResearchForm {
    pub entity_id: Option<i64>,
    pub abs_path: Option<PathBuf>,
    pub name: String,
    /// Kind from frontmatter `type:` (concept / faction / item / …).
    pub kind: String,
    pub aliases: String,
    pub summary: String,
    pub notes: String,
    pub last_saved_hash: u64,
}

#[derive(Debug)]
pub(crate) struct ResearchView {
    cards: Vec<ResearchCard>,
    form: Option<ResearchForm>,
    last_edit: Option<Instant>,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Select(i64),
    NameChanged(String),
    KindChanged(String),
    AliasesChanged(String),
    SummaryChanged(String),
    NotesChanged(String),
    SaveTick,
    Saved(Result<(), String>),
}

pub(crate) struct ViewReaction {
    pub fs_changed: bool,
    pub task: Task<Message>,
}

impl Default for ViewReaction {
    fn default() -> Self {
        Self { fs_changed: false, task: Task::none() }
    }
}

impl std::fmt::Debug for ViewReaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewReaction")
            .field("fs_changed", &self.fs_changed)
            .finish_non_exhaustive()
    }
}

impl ResearchView {
    pub(crate) const fn new() -> Self {
        Self { cards: Vec::new(), form: None, last_edit: None }
    }

    pub(crate) fn refresh_cards(&mut self, project: &Project) {
        self.cards = load_cards(project);
        if let Some(form) = &self.form {
            if let Some(id) = form.entity_id {
                if !self.cards.iter().any(|c| c.entity_id == id) {
                    self.form = None;
                }
            }
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn update(
        &mut self,
        message: Message,
        project: Option<&Project>,
        project_root: Option<&std::path::Path>,
    ) -> ViewReaction {
        match message {
            Message::Select(id) => {
                if let (Some(p), Some(root)) = (project, project_root) {
                    self.form = load_form(p, root, id);
                    self.last_edit = None;
                }
                ViewReaction::default()
            }
            Message::NameChanged(s) => self.edit(|f| f.name = s),
            Message::KindChanged(s) => self.edit(|f| f.kind = s),
            Message::AliasesChanged(s) => self.edit(|f| f.aliases = s),
            Message::SummaryChanged(s) => self.edit(|f| f.summary = s),
            Message::NotesChanged(s) => self.edit(|f| f.notes = s),
            Message::SaveTick => self.save_if_idle(project_root),
            Message::Saved(Ok(())) => {
                if let Some(form) = self.form.as_mut() {
                    form.last_saved_hash = hash_form(form);
                }
                ViewReaction { fs_changed: true, ..Default::default() }
            }
            Message::Saved(Err(err)) => {
                tracing::error!(%err, "research save failed");
                ViewReaction::default()
            }
        }
    }

    fn edit(&mut self, mutate: impl FnOnce(&mut ResearchForm)) -> ViewReaction {
        if let Some(form) = self.form.as_mut() {
            mutate(form);
            self.last_edit = Some(Instant::now());
            ViewReaction {
                task: Task::perform(
                    async {
                        tokio::time::sleep(SAVE_IDLE).await;
                    },
                    |()| Message::SaveTick,
                ),
                ..Default::default()
            }
        } else {
            ViewReaction::default()
        }
    }

    fn save_if_idle(&mut self, project_root: Option<&std::path::Path>) -> ViewReaction {
        let Some(last) = self.last_edit else {
            return ViewReaction::default();
        };
        if last.elapsed() < SAVE_IDLE {
            return ViewReaction::default();
        }
        let Some(form) = self.form.as_ref() else {
            return ViewReaction::default();
        };
        if hash_form(form) == form.last_saved_hash {
            return ViewReaction::default();
        }
        let Some(_root) = project_root else {
            return ViewReaction::default();
        };
        let form_owned = form.clone();
        self.last_edit = None;
        ViewReaction {
            task: Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || persist(&form_owned))
                        .await
                        .map_err(|e| format!("join error: {e}"))
                        .and_then(|res| res.map_err(|e| e.to_string()))
                },
                Message::Saved,
            ),
            ..Default::default()
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let cards_pane = scrollable(card_list(
            &self.cards,
            self.form.as_ref().and_then(|f| f.entity_id),
        ))
        .width(Length::FillPortion(1))
        .height(Length::Fill);
        let editor_pane: Element<'_, Message> = self.form.as_ref().map_or_else(
            || {
                container(text("Select a research note on the left to edit it.").size(13))
                    .padding(24)
                    .into()
            },
            form_view,
        );
        let editor_pane = container(editor_pane)
            .width(Length::FillPortion(2))
            .height(Length::Fill);
        row![cards_pane, editor_pane].spacing(8).height(Length::Fill).into()
    }
}

fn card_list(cards: &[ResearchCard], selected: Option<i64>) -> Element<'_, Message> {
    if cards.is_empty() {
        return column![
            text("No research notes yet.").size(13),
            text(
                "Add Markdown files under Research/. Frontmatter `type:` can \
                 be `concept` (default), `faction`, or `item` — the rest is \
                 free-form notes.",
            )
            .size(11),
        ]
        .spacing(6)
        .padding(12)
        .into();
    }
    let mut col = column![].spacing(4).padding(8);
    for card in cards {
        let id = card.entity_id;
        let style = if Some(id) == selected {
            button::primary
        } else {
            button::secondary
        };
        let label = format!("{}\n[{}]", card.name, card.kind);
        col = col.push(
            button(text(label).size(12))
                .on_press(Message::Select(id))
                .style(style)
                .width(Length::Fill),
        );
    }
    col.into()
}

fn form_view(form: &ResearchForm) -> Element<'_, Message> {
    let mut col = column![].spacing(8).padding(16);
    col = col.push(
        column![
            text("Name").size(11),
            text_input("Name of this note", &form.name)
                .on_input(Message::NameChanged)
                .size(13),
        ]
        .spacing(2),
    );
    col = col.push(
        column![
            text("Kind (concept / faction / item)").size(11),
            text_input("concept", &form.kind)
                .on_input(Message::KindChanged)
                .size(13),
        ]
        .spacing(2),
    );
    col = col.push(
        column![
            text("Aliases (comma-separated)").size(11),
            text_input("…", &form.aliases)
                .on_input(Message::AliasesChanged)
                .size(13),
        ]
        .spacing(2),
    );
    col = col.push(
        column![
            text("Summary").size(11),
            text_input("One-line summary", &form.summary)
                .on_input(Message::SummaryChanged)
                .size(13),
        ]
        .spacing(2),
    );
    col = col.push(horizontal_rule(1));
    col = col.push(text("Notes").size(11));
    col = col.push(
        text_input("Free-form notes (saved to the file's body)", &form.notes)
            .on_input(Message::NotesChanged)
            .size(12),
    );
    scrollable(col).height(Length::Fill).width(Length::Fill).into()
}

fn load_cards(project: &Project) -> Vec<ResearchCard> {
    let conn = project.database().conn();
    // Research entities are anything that came in from a Research/ file —
    // detect via document's rel_path prefix.
    let mut stmt = match conn.prepare(
        "SELECT e.id, e.name, e.kind
           FROM entities e
           JOIN documents d ON d.id = e.document_id
          WHERE e.project_id = ?1 AND d.rel_path LIKE 'Research/%'
          ORDER BY e.name",
    ) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "research card query failed");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(params![project.id()], |r| {
        Ok(ResearchCard {
            entity_id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
        })
    });
    rows.map(|it| it.flatten().collect()).unwrap_or_default()
}

fn load_form(
    project: &Project,
    project_root: &std::path::Path,
    entity_id: i64,
) -> Option<ResearchForm> {
    let conn = project.database().conn();
    let row: rusqlite::Result<(String, String, String, Option<String>)> = conn.query_row(
        "SELECT name, kind, data_json,
                (SELECT rel_path FROM documents WHERE id = entities.document_id)
           FROM entities WHERE id = ?1",
        params![entity_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    );
    let Ok((name, kind, data_json, rel_path)) = row else {
        return None;
    };
    let abs_path = rel_path.as_ref().map(|r| project_root.join(r));
    let notes = abs_path
        .as_ref()
        .and_then(|p| Document::load(project_root, p).ok())
        .map(|d| d.body)
        .unwrap_or_default();
    let aliases = load_aliases(project, entity_id);
    let summary = read_string_field(&data_json, "summary");
    let mut form = ResearchForm {
        entity_id: Some(entity_id),
        abs_path,
        name,
        kind,
        aliases,
        summary,
        notes,
        last_saved_hash: 0,
    };
    form.last_saved_hash = hash_form(&form);
    Some(form)
}

fn load_aliases(project: &Project, entity_id: i64) -> String {
    project
        .database()
        .conn()
        .query_row(
            "SELECT aliases_json FROM entities WHERE id = ?1",
            params![entity_id],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .map(|v| v.join(", "))
        .unwrap_or_default()
}

fn read_string_field(data_json: &str, key: &str) -> String {
    serde_json::from_str::<serde_json::Value>(data_json)
        .ok()
        .as_ref()
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned()
}

fn persist(form: &ResearchForm) -> letswrite_core::Result<()> {
    let Some(abs_path) = form.abs_path.as_ref() else {
        return Ok(());
    };
    let project_root = abs_path
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| letswrite_core::Error::InvalidData(
            "research file has no project root".to_owned(),
        ))?;
    let rel_path = abs_path
        .strip_prefix(project_root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    let mut frontmatter = serde_yaml::Mapping::new();
    insert_str(&mut frontmatter, "title", &form.name);
    insert_str(
        &mut frontmatter,
        "type",
        if form.kind.is_empty() { "concept" } else { &form.kind },
    );
    if !form.summary.is_empty() {
        insert_str(&mut frontmatter, "summary", &form.summary);
    }
    let alias_list: Vec<YamlValue> = form
        .aliases
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| YamlValue::String(s.to_owned()))
        .collect();
    if !alias_list.is_empty() {
        frontmatter.insert(
            YamlValue::String("aliases".to_owned()),
            YamlValue::Sequence(alias_list),
        );
    }

    let document = Document {
        rel_path,
        kind: Some(DocumentKind::Research),
        title: form.name.clone(),
        frontmatter: YamlValue::Mapping(frontmatter),
        body: form.notes.clone(),
    };
    document.save(project_root)
}

fn insert_str(map: &mut serde_yaml::Mapping, key: &str, value: &str) {
    map.insert(
        YamlValue::String(key.to_owned()),
        YamlValue::String(value.to_owned()),
    );
}

fn hash_form(form: &ResearchForm) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    form.name.hash(&mut h);
    form.kind.hash(&mut h);
    form.aliases.hash(&mut h);
    form.summary.hash(&mut h);
    form.notes.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persist_round_trips_with_default_kind() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let research = root.join("Research");
        std::fs::create_dir_all(&research).unwrap();
        let form = ResearchForm {
            entity_id: Some(1),
            abs_path: Some(research.join("Power as Narrative Arc.md")),
            name: "Power as Narrative Arc".into(),
            kind: String::new(), // empty -> defaults to "concept"
            aliases: "Narrative power".into(),
            summary: "Why story shape determines who feels powerful.".into(),
            notes: "# Power as Narrative Arc\n\nNotes…\n".into(),
            last_saved_hash: 0,
        };
        persist(&form).unwrap();
        let written =
            std::fs::read_to_string(research.join("Power as Narrative Arc.md")).unwrap();
        assert!(written.contains("title: Power as Narrative Arc"));
        assert!(written.contains("type: concept"));
        assert!(written.contains("- Narrative power"));
        assert!(written.contains("summary:"));
    }

    #[test]
    fn persist_honours_explicit_kind() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let research = root.join("Research");
        std::fs::create_dir_all(&research).unwrap();
        let form = ResearchForm {
            entity_id: Some(1),
            abs_path: Some(research.join("Strategic Integrity Unit Leadership.md")),
            name: "Strategic Integrity Unit Leadership".into(),
            kind: "faction".into(),
            ..Default::default()
        };
        persist(&form).unwrap();
        let written = std::fs::read_to_string(
            research.join("Strategic Integrity Unit Leadership.md"),
        )
        .unwrap();
        assert!(written.contains("type: faction"));
    }
}
