//! Character overview & structured editor.
//!
//! Lists every character entity in the project, lets the user pick one,
//! and offers a structured editor backed by both the on-disk Markdown
//! file (frontmatter + body notes) and the `SQLite` index (so reindex
//! after save keeps the entity row in sync with the file).
//!
//! Persistence model: typing into the structured fields updates an
//! in-memory `CharacterForm`. A debounced save writes the form back to
//! the character's Markdown file using the existing `Document` round-trip
//! (frontmatter for structured fields, body for the free-form notes).
//! The mention index is rebuilt by the next importer run — see
//! [`crate::app`].

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::widget::{button, column, container, horizontal_rule, row, scrollable, text, text_input};
use iced::{Element, Length, Task};

use letswrite_core::{Document, DocumentKind, Project};
use rusqlite::params;
use serde_yaml::Value as YamlValue;

const SAVE_IDLE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub(crate) struct CharacterCard {
    pub entity_id: i64,
    pub name: String,
    pub role: String,
}

/// Editable representation of one character's structured frontmatter
/// fields. `notes` is the document body below the frontmatter.
#[derive(Debug, Clone, Default)]
pub(crate) struct CharacterForm {
    pub entity_id: Option<i64>,
    pub abs_path: Option<PathBuf>,
    pub name: String,
    pub aliases: String, // comma-separated
    pub role: String,
    pub traits: String,  // newline-separated
    pub blind_spot: String,
    pub arc: String,
    pub notes: String,
    pub timeline: Vec<TimelineRow>,
    /// Hash of the last-saved form snapshot; used to skip needless writes.
    pub last_saved_hash: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct TimelineRow {
    pub scene_title: String,
    pub field: String,
    pub value: String,
}

#[derive(Debug)]
pub(crate) struct CharactersView {
    cards: Vec<CharacterCard>,
    /// Currently-loaded form. `None` when nothing selected.
    form: Option<CharacterForm>,
    /// Most recent edit timestamp; drives debounced autosave.
    last_edit: Option<Instant>,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Select(i64),
    NameChanged(String),
    AliasesChanged(String),
    RoleChanged(String),
    TraitsChanged(String),
    BlindSpotChanged(String),
    ArcChanged(String),
    NotesChanged(String),
    /// Idle-debounce tick; if no edits have arrived for `SAVE_IDLE`, write.
    SaveTick,
    /// Background save completed.
    Saved(Result<(), String>),
}

pub(crate) struct ViewReaction {
    /// File on disk was rewritten — shell should reindex + re-detect.
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

impl CharactersView {
    pub(crate) const fn new() -> Self {
        Self { cards: Vec::new(), form: None, last_edit: None }
    }

    /// Re-query the project's character list. Called by the shell when the
    /// view is opened or after a save.
    pub(crate) fn refresh_cards(&mut self, project: &Project) {
        self.cards = load_character_cards(project);
        // Drop the loaded form if its entity no longer exists.
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
            Message::RoleChanged(s) => self.edit(|f| f.role = s),
            Message::TraitsChanged(s) => self.edit(|f| f.traits = s),
            Message::BlindSpotChanged(s) => self.edit(|f| f.blind_spot = s),
            Message::ArcChanged(s) => self.edit(|f| f.arc = s),
            Message::NotesChanged(s) => self.edit(|f| f.notes = s),
            Message::SaveTick => self.save_if_idle(project_root),
            Message::Saved(Ok(())) => {
                if let Some(form) = self.form.as_mut() {
                    form.last_saved_hash = hash_form(form);
                    tracing::debug!("character form autosaved");
                }
                ViewReaction { fs_changed: true, ..Default::default() }
            }
            Message::Saved(Err(err)) => {
                tracing::error!(%err, "character form save failed");
                ViewReaction::default()
            }
        }
    }

    fn edit(&mut self, mutate: impl FnOnce(&mut CharacterForm)) -> ViewReaction {
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
        let cards_pane = scrollable(card_list(&self.cards, self.form.as_ref().and_then(|f| f.entity_id)))
            .width(Length::FillPortion(1))
            .height(Length::Fill);
        let editor_pane: Element<'_, Message> = self.form.as_ref().map_or_else(
            || {
                container(
                    text("Select a character on the left to edit them.").size(13),
                )
                .padding(24)
                .into()
            },
            form_view,
        );
        let editor_pane = container(editor_pane)
            .width(Length::FillPortion(2))
            .height(Length::Fill);

        row![cards_pane, editor_pane]
            .spacing(8)
            .height(Length::Fill)
            .into()
    }
}

fn card_list(
    cards: &[CharacterCard],
    selected: Option<i64>,
) -> Element<'_, Message> {
    if cards.is_empty() {
        return column![
            text("No characters yet.").size(13),
            text(
                "Add a Markdown file under Characters/ — its frontmatter \
                 becomes the character's structured fields.",
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
        let label = if card.role.is_empty() {
            card.name.clone()
        } else {
            format!("{}\n{}", card.name, card.role)
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

fn form_view(form: &CharacterForm) -> Element<'_, Message> {
    let mut col = column![]
        .spacing(8)
        .padding(16);

    col = col.push(
        column![
            text("Name").size(11),
            text_input("Character name", &form.name)
                .on_input(Message::NameChanged)
                .size(13),
        ]
        .spacing(2),
    );
    col = col.push(
        column![
            text("Aliases (comma-separated)").size(11),
            text_input("Alex, Calder, …", &form.aliases)
                .on_input(Message::AliasesChanged)
                .size(13),
        ]
        .spacing(2),
    );
    col = col.push(
        column![
            text("Role").size(11),
            text_input("Deputy director, antagonist, …", &form.role)
                .on_input(Message::RoleChanged)
                .size(13),
        ]
        .spacing(2),
    );
    col = col.push(
        column![
            text("Traits (one per line)").size(11),
            text_input("Calm under pressure", &form.traits)
                .on_input(Message::TraitsChanged)
                .size(13),
        ]
        .spacing(2),
    );
    col = col.push(
        column![
            text("Blind spot").size(11),
            text_input(
                "What they don't see about themselves or the system",
                &form.blind_spot,
            )
            .on_input(Message::BlindSpotChanged)
            .size(13),
        ]
        .spacing(2),
    );
    col = col.push(
        column![
            text("Arc").size(11),
            text_input("Where they start; where they land", &form.arc)
                .on_input(Message::ArcChanged)
                .size(13),
        ]
        .spacing(2),
    );

    col = col.push(horizontal_rule(1));
    col = col.push(text("Notes").size(11));
    col = col.push(
        text_input("Free-form notes (saved to the character file's body)", &form.notes)
            .on_input(Message::NotesChanged)
            .size(12),
    );

    if !form.timeline.is_empty() {
        col = col.push(horizontal_rule(1));
        col = col.push(text("Timeline").size(11));
        for entry in &form.timeline {
            col = col.push(
                container(
                    column![
                        text(format!("{} — {}", entry.scene_title, entry.field)).size(11),
                        text(entry.value.clone()).size(11),
                    ]
                    .spacing(2),
                )
                .padding(6),
            );
        }
    }

    scrollable(col).height(Length::Fill).width(Length::Fill).into()
}

fn load_character_cards(project: &Project) -> Vec<CharacterCard> {
    let conn = project.database().conn();
    let mut stmt = match conn.prepare(
        "SELECT e.id, e.name, e.data_json
           FROM entities e
          WHERE e.project_id = ?1 AND e.kind = 'character'
          ORDER BY e.name",
    ) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "character list query failed");
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
            tracing::warn!(%err, "character list iteration failed");
            return Vec::new();
        }
    };
    rows.flatten()
        .map(|(entity_id, name, data_json)| CharacterCard {
            entity_id,
            name,
            role: read_string_field(&data_json, "role"),
        })
        .collect()
}

fn load_form(
    project: &Project,
    project_root: &std::path::Path,
    entity_id: i64,
) -> Option<CharacterForm> {
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
    let role = read_string_field(&data_json, "role");
    let traits = read_traits_field(&data_json);
    let blind_spot = read_string_field(&data_json, "blind_spot");
    let arc = read_string_field(&data_json, "arc");
    let timeline = load_timeline(project, entity_id);

    let mut form = CharacterForm {
        entity_id: Some(entity_id),
        abs_path,
        name,
        aliases,
        role,
        traits,
        blind_spot,
        arc,
        notes,
        timeline,
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

fn load_timeline(project: &Project, entity_id: i64) -> Vec<TimelineRow> {
    let conn = project.database().conn();
    let mut stmt = match conn.prepare(
        "SELECT COALESCE(s.synopsis, ''), te.field, te.value
           FROM timeline_entries te
           LEFT JOIN scenes s ON s.id = te.scene_id
          WHERE te.entity_id = ?1
          ORDER BY te.created_at",
    ) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "timeline query failed to prepare");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(params![entity_id], |r| {
        Ok(TimelineRow {
            scene_title: r.get(0)?,
            field: r.get(1)?,
            value: r.get(2)?,
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

/// Extract a `traits:` array from `data_json` into newline-separated text.
/// Tolerant of missing key, scalar, or non-string items.
fn read_traits_field(data_json: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data_json) else {
        return String::new();
    };
    let Some(arr) = value.get("traits").and_then(|v| v.as_array()) else {
        return String::new();
    };
    arr.iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Serialize a `CharacterForm` back to disk: rebuild the frontmatter from
/// the structured fields, keep the user's free-form body verbatim.
fn persist(form: &CharacterForm) -> letswrite_core::Result<()> {
    let Some(abs_path) = form.abs_path.as_ref() else {
        return Ok(());
    };
    let project_root = abs_path
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| letswrite_core::Error::InvalidData(
            "character file has no project root".to_owned(),
        ))?;
    let rel_path = abs_path
        .strip_prefix(project_root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    let mut frontmatter = serde_yaml::Mapping::new();
    insert_str(&mut frontmatter, "title", &form.name);
    insert_str(&mut frontmatter, "type", "character");
    if !form.role.is_empty() {
        insert_str(&mut frontmatter, "role", &form.role);
    }
    if !form.blind_spot.is_empty() {
        insert_str(&mut frontmatter, "blind_spot", &form.blind_spot);
    }
    if !form.arc.is_empty() {
        insert_str(&mut frontmatter, "arc", &form.arc);
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
    let traits_list: Vec<YamlValue> = form
        .traits
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| YamlValue::String(s.to_owned()))
        .collect();
    if !traits_list.is_empty() {
        frontmatter.insert(
            YamlValue::String("traits".to_owned()),
            YamlValue::Sequence(traits_list),
        );
    }

    let document = Document {
        rel_path,
        kind: Some(DocumentKind::Character),
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

/// Hash the form's user-editable fields so a no-op tick doesn't write.
fn hash_form(form: &CharacterForm) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    form.name.hash(&mut h);
    form.aliases.hash(&mut h);
    form.role.hash(&mut h);
    form.traits.hash(&mut h);
    form.blind_spot.hash(&mut h);
    form.arc.hash(&mut h);
    form.notes.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_string_field_handles_missing() {
        assert_eq!(read_string_field("{}", "role"), "");
        assert_eq!(
            read_string_field(r#"{"role":"protagonist"}"#, "role"),
            "protagonist"
        );
        assert_eq!(read_string_field("not-json", "role"), "");
    }

    #[test]
    fn read_traits_field_joins_with_newlines() {
        let data = r#"{"traits":["calm under pressure","ethically alert"]}"#;
        let traits = read_traits_field(data);
        assert!(traits.contains("calm under pressure"));
        assert!(traits.contains('\n'));
    }

    #[test]
    fn read_traits_returns_empty_when_absent() {
        assert_eq!(read_traits_field("{}"), "");
        assert_eq!(read_traits_field(r#"{"traits":"not-array"}"#), "");
    }

    #[test]
    fn persist_writes_frontmatter_then_body() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path();
        let chars_dir = project_root.join("Characters");
        std::fs::create_dir_all(&chars_dir).unwrap();
        let form = CharacterForm {
            entity_id: Some(1),
            abs_path: Some(chars_dir.join("Evan Calder.md")),
            name: "Evan Calder".to_owned(),
            aliases: "Evan, Calder".to_owned(),
            role: "Deputy director".to_owned(),
            traits: "Calm under pressure\nEthically alert".to_owned(),
            blind_spot: "Believes managed harm is morally distinct from enabling".to_owned(),
            arc: "Becomes indispensable, accepts blame".to_owned(),
            notes: "# Evan\n\nFurther notes here.\n".to_owned(),
            timeline: Vec::new(),
            last_saved_hash: 0,
        };
        persist(&form).unwrap();
        let written =
            std::fs::read_to_string(chars_dir.join("Evan Calder.md")).unwrap();
        assert!(written.starts_with("---"));
        assert!(written.contains("title: Evan Calder"));
        assert!(written.contains("- Evan"));
        assert!(written.contains("- Calder"));
        assert!(written.contains("- Calm under pressure"));
        assert!(written.contains("# Evan"));
    }

    #[test]
    fn hash_form_distinguishes_changes() {
        let a = CharacterForm { name: "Evan".into(), ..CharacterForm::default() };
        let h1 = hash_form(&a);
        let b = CharacterForm { role: "Protagonist".into(), ..a };
        let h2 = hash_form(&b);
        assert_ne!(h1, h2);
    }
}
