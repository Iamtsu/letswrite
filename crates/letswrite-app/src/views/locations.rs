//! Location overview & structured editor.
//!
//! Mirrors [`super::characters`] but for `kind='location'` entities. Each
//! location has a name, optional aliases, a "where" description, a "how
//! to get there" note, the list of characters who visit it (derived from
//! `entity_mentions` joined to scenes), and free-form notes.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::widget::{button, column, container, row, rule, scrollable, text, text_input};
use iced::{Element, Length, Task};

use letswrite_core::{Document, DocumentKind, Project};
use rusqlite::params;
use serde_yaml::Value as YamlValue;

const SAVE_IDLE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub(crate) struct LocationCard {
    pub entity_id: i64,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LocationForm {
    pub entity_id: Option<i64>,
    pub abs_path: Option<PathBuf>,
    pub name: String,
    pub aliases: String,
    pub description: String,
    pub how_to_get_there: String,
    pub notes: String,
    /// Characters that visit this location, derived from `entity_mentions`
    /// joined to scenes. Read-only here; edited indirectly by tagging
    /// wiki-links in prose.
    pub visitors: Vec<VisitorRow>,
    pub last_saved_hash: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct VisitorRow {
    pub character_name: String,
    pub scene_title: String,
    pub document_title: String,
}

#[derive(Debug)]
pub(crate) struct LocationsView {
    cards: Vec<LocationCard>,
    form: Option<LocationForm>,
    last_edit: Option<Instant>,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Select(i64),
    NameChanged(String),
    AliasesChanged(String),
    DescriptionChanged(String),
    HowToGetThereChanged(String),
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

impl LocationsView {
    pub(crate) const fn new() -> Self {
        Self { cards: Vec::new(), form: None, last_edit: None }
    }

    pub(crate) fn refresh_cards(&mut self, project: &Project) {
        self.cards = load_location_cards(project);
        if let Some(form) = &self.form {
            if let Some(id) = form.entity_id {
                if !self.cards.iter().any(|c| c.entity_id == id) {
                    self.form = None;
                }
            }
        }
    }

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
            Message::AliasesChanged(s) => self.edit(|f| f.aliases = s),
            Message::DescriptionChanged(s) => self.edit(|f| f.description = s),
            Message::HowToGetThereChanged(s) => self.edit(|f| f.how_to_get_there = s),
            Message::NotesChanged(s) => self.edit(|f| f.notes = s),
            Message::SaveTick => self.save_if_idle(project_root),
            Message::Saved(Ok(())) => {
                if let Some(form) = self.form.as_mut() {
                    form.last_saved_hash = hash_form(form);
                    tracing::debug!("location form autosaved");
                }
                ViewReaction { fs_changed: true, ..Default::default() }
            }
            Message::Saved(Err(err)) => {
                tracing::error!(%err, "location form save failed");
                ViewReaction::default()
            }
        }
    }

    fn edit(&mut self, mutate: impl FnOnce(&mut LocationForm)) -> ViewReaction {
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
                container(
                    text("Select a location on the left to edit it.").size(13),
                )
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

fn card_list(cards: &[LocationCard], selected: Option<i64>) -> Element<'_, Message> {
    if cards.is_empty() {
        return column![
            text("No locations yet.").size(13),
            text(
                "Add a Markdown file under Locations/ — its frontmatter \
                 becomes the location's structured fields.",
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
        let label = if card.description.is_empty() {
            card.name.clone()
        } else {
            // First line of description, clipped.
            let first = card
                .description
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect::<String>();
            format!("{}\n{}", card.name, first)
        };
        col = col.push(
            button(text(label).size(12))
                .on_press(Message::Select(id))
                .style(style)
                .width(Length::Fill),
        );
    }
    col.into()
}

fn form_view(form: &LocationForm) -> Element<'_, Message> {
    let mut col = column![].spacing(8).padding(16);

    col = col.push(
        column![
            text("Name").size(11),
            text_input("Location name", &form.name)
                .on_input(Message::NameChanged)
                .size(13),
        ]
        .spacing(2),
    );
    col = col.push(
        column![
            text("Aliases (comma-separated)").size(11),
            text_input("the building, the office, …", &form.aliases)
                .on_input(Message::AliasesChanged)
                .size(13),
        ]
        .spacing(2),
    );
    col = col.push(
        column![
            text("Description").size(11),
            text_input(
                "Where it is, what it feels like",
                &form.description,
            )
            .on_input(Message::DescriptionChanged)
            .size(13),
        ]
        .spacing(2),
    );
    col = col.push(
        column![
            text("How to get there").size(11),
            text_input(
                "Travel logistics, access controls, …",
                &form.how_to_get_there,
            )
            .on_input(Message::HowToGetThereChanged)
            .size(13),
        ]
        .spacing(2),
    );

    col = col.push(rule::horizontal(1.0));
    col = col.push(text("Notes").size(11));
    col = col.push(
        text_input("Free-form notes (saved to the body of the file)", &form.notes)
            .on_input(Message::NotesChanged)
            .size(12),
    );

    if !form.visitors.is_empty() {
        col = col.push(rule::horizontal(1.0));
        col = col.push(text("Visitors").size(11));
        for v in &form.visitors {
            col = col.push(
                container(
                    column![
                        text(format!("{} — {}", v.character_name, v.scene_title))
                            .size(11),
                        text(v.document_title.clone()).size(10),
                    ]
                    .spacing(2),
                )
                .padding(6),
            );
        }
    }

    scrollable(col).height(Length::Fill).width(Length::Fill).into()
}

fn load_location_cards(project: &Project) -> Vec<LocationCard> {
    let conn = project.database().conn();
    let mut stmt = match conn.prepare(
        "SELECT e.id, e.name, e.data_json
           FROM entities e
          WHERE e.project_id = ?1 AND e.kind = 'location'
          ORDER BY e.name",
    ) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "location list query failed");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(params![project.id()], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(%err, "location list iteration failed");
            return Vec::new();
        }
    };
    rows.flatten()
        .map(|(entity_id, name, data_json)| LocationCard {
            entity_id,
            name,
            description: read_string_field(&data_json, "description"),
        })
        .collect()
}

fn load_form(
    project: &Project,
    project_root: &std::path::Path,
    entity_id: i64,
) -> Option<LocationForm> {
    let conn = project.database().conn();
    let row: rusqlite::Result<(String, String, Option<String>)> = conn.query_row(
        "SELECT name, data_json, (SELECT rel_path FROM documents WHERE id = entities.document_id)
           FROM entities WHERE id = ?1",
        params![entity_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    );
    let Ok((name, data_json, rel_path)) = row else {
        return None;
    };
    let abs_path = rel_path.as_ref().map(|r| project_root.join(r));
    let notes = abs_path
        .as_ref()
        .and_then(|p| Document::load(project_root, p).ok())
        .map(|d| d.body)
        .unwrap_or_default();
    let aliases = load_aliases(project, entity_id);
    let description = read_string_field(&data_json, "description");
    let how_to_get_there = read_string_field(&data_json, "how_to_get_there");
    let visitors = load_visitors(project, entity_id);

    let mut form = LocationForm {
        entity_id: Some(entity_id),
        abs_path,
        name,
        aliases,
        description,
        how_to_get_there,
        notes,
        visitors,
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

/// Visitors = characters mentioned in scenes whose location is this entity.
fn load_visitors(project: &Project, location_id: i64) -> Vec<VisitorRow> {
    let conn = project.database().conn();
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT char_e.name, COALESCE(s.synopsis, ''), d.title
           FROM scenes s
           JOIN documents d ON d.id = s.document_id
           JOIN entity_mentions em ON em.document_id = d.id
           JOIN entities char_e ON char_e.id = em.entity_id
          WHERE s.location_entity_id = ?1
            AND char_e.kind = 'character'
          ORDER BY char_e.name, s.order_index",
    ) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "visitors query failed to prepare");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(params![location_id], |r| {
        Ok(VisitorRow {
            character_name: r.get(0)?,
            scene_title: r.get(1)?,
            document_title: r.get(2)?,
        })
    });
    rows.map(|it| it.flatten().collect()).unwrap_or_default()
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

fn persist(form: &LocationForm) -> letswrite_core::Result<()> {
    let Some(abs_path) = form.abs_path.as_ref() else {
        return Ok(());
    };
    let project_root = abs_path
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| letswrite_core::Error::InvalidData(
            "location file has no project root".to_owned(),
        ))?;
    let rel_path = abs_path
        .strip_prefix(project_root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    let mut frontmatter = serde_yaml::Mapping::new();
    insert_str(&mut frontmatter, "title", &form.name);
    insert_str(&mut frontmatter, "type", "location");
    if !form.description.is_empty() {
        insert_str(&mut frontmatter, "description", &form.description);
    }
    if !form.how_to_get_there.is_empty() {
        insert_str(&mut frontmatter, "how_to_get_there", &form.how_to_get_there);
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
        kind: Some(DocumentKind::Location),
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

fn hash_form(form: &LocationForm) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    form.name.hash(&mut h);
    form.aliases.hash(&mut h);
    form.description.hash(&mut h);
    form.how_to_get_there.hash(&mut h);
    form.notes.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persist_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let locs = root.join("Locations");
        std::fs::create_dir_all(&locs).unwrap();
        let form = LocationForm {
            entity_id: Some(1),
            abs_path: Some(locs.join("Strategic Integrity Unit.md")),
            name: "Strategic Integrity Unit".into(),
            aliases: "SIU, the office".into(),
            description: "Government office in the K Street corridor.".into(),
            how_to_get_there: "Badge access, third floor.".into(),
            notes: "# SIU\n\nRoom layout: …\n".into(),
            visitors: Vec::new(),
            last_saved_hash: 0,
        };
        persist(&form).unwrap();
        let written =
            std::fs::read_to_string(locs.join("Strategic Integrity Unit.md")).unwrap();
        assert!(written.contains("title: Strategic Integrity Unit"));
        assert!(written.contains("type: location"));
        assert!(written.contains("- SIU"));
        assert!(written.contains("- the office"));
        assert!(written.contains("how_to_get_there"));
        assert!(written.contains("# SIU"));
    }

    #[test]
    fn read_string_field_handles_missing() {
        assert_eq!(read_string_field("{}", "description"), "");
        assert_eq!(
            read_string_field(r#"{"description":"office"}"#, "description"),
            "office"
        );
    }
}
