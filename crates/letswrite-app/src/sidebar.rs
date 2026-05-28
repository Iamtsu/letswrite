//! Project sidebar: pick a project root, browse its files, open one in the
//! editor, and perform basic file-management operations (new / rename /
//! delete).
//!
//! The sidebar holds a lightweight tree of `(folder kind, files)` derived
//! from `letswrite_core::Project::scan`. It does NOT own the `Project`
//! itself — the app shell does — but it asks the app for refreshes by
//! emitting a [`Message::RefreshRequested`] after any mutating action.
//!
//! Drag-to-reorder is not in v1 — files are ordered naturally by name on
//! disk. Writers can prefix with numeric ordering (`Chapter 02 — …`) for
//! control.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use iced::widget::{
    button, column, container, row, rule, scrollable, text, text_input, tooltip,
};
use iced::widget::tooltip::Position as TooltipPosition;
use iced::{Element, Length, Task};

use letswrite_core::DocumentKind;

use crate::search::{self, Match, SearchState};

/// One file shown in the tree.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub abs_path: PathBuf,
    pub display_name: String,
}

/// Group of entries for one folder kind (e.g. Chapters, Characters).
#[derive(Debug, Clone, Default)]
pub(crate) struct Group {
    pub entries: Vec<Entry>,
    pub expanded: bool,
}

#[derive(Debug)]
pub(crate) struct Sidebar {
    project_root: Option<PathBuf>,
    project_name: String,
    groups: BTreeMap<DocumentKind, Group>,
    /// File-management ui state: a transient input dialog for new/rename.
    dialog: Option<Dialog>,
    /// Which side-pane tab is currently visible.
    tab: Tab,
    /// Find-in-document panel state (lives in the Search tab).
    search: SearchState,
}

/// Top-level sections of the left sidebar. The Project tab carries the
/// file tree and create/rename/delete dialogs (the original sidebar).
/// The Search tab hosts find-in-document UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Project,
    Search,
}

#[derive(Debug, Clone)]
enum Dialog {
    /// Confirm before deleting `abs_path`.
    ConfirmDelete { abs_path: PathBuf, display: String },
    /// Inline create — the entered name will be saved in `<kind>/<name>.md`.
    CreateDocument { kind: DocumentKind, draft: String },
    /// Inline rename — the entered name replaces the file stem.
    RenameDocument { abs_path: PathBuf, draft: String },
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    /// User asked to pick a project directory.
    PickProject,
    /// User asked to (re-)run the Markdown files importer on the open project.
    Reimport,
    /// User wants to switch the main view (Editor / Characters / ...).
    ShowEditor,
    ShowCharacters,
    ShowLocations,
    ShowCorkboard,
    ShowTimeline,
    ShowRelationships,
    ShowResearch,
    /// Result of the file-picker future. Handled by the app shell, which
    /// opens the project and then fires [`Self::ProjectLoaded`] with the
    /// real scan back into us.
    ProjectPicked(Option<PathBuf>),
    /// Shell-only: the app finished opening a project and is handing us
    /// the scan to populate the tree. The sidebar never emits this on its
    /// own — only the shell does — so this is a one-way ingress.
    ProjectLoaded {
        root: PathBuf,
        name: String,
        scan: Vec<(DocumentKind, PathBuf)>,
    },
    /// Re-collapse / expand a folder group.
    ToggleGroup(DocumentKind),
    /// Open this file in the editor (handled by the app shell).
    Open(PathBuf),
    /// Show the inline "new document" dialog under a folder.
    NewDocumentPrompt(DocumentKind),
    /// User typed in the new-document dialog.
    NewDocumentDraftChanged(String),
    /// Commit the new document creation.
    NewDocumentSubmit,
    /// Show the rename dialog for a file.
    RenamePrompt(PathBuf),
    /// User typed in the rename dialog.
    RenameDraftChanged(String),
    /// Commit the rename.
    RenameSubmit,
    /// Show the delete confirmation for a file.
    DeletePrompt(PathBuf),
    /// User confirmed deletion.
    DeleteConfirm,
    /// User dismissed any open dialog.
    DialogDismiss,
    /// Switch the sidebar's visible tab (Project / Search).
    TabSelected(Tab),
    /// Sub-message for the Search panel.
    Search(search::Message),
}

/// What the sidebar wants the app shell to do after handling a message.
pub(crate) struct SidebarReaction {
    /// File to open in the editor (after rename, this points at the new path).
    pub open: Option<PathBuf>,
    /// User picked a folder — shell should open it as a project.
    pub open_project: Option<PathBuf>,
    /// Filesystem mutated — app should reindex and re-scan.
    pub fs_changed: bool,
    /// User asked to (re-)run the importer.
    pub reimport_requested: bool,
    /// User wants to switch the main view.
    pub show_view: Option<crate::views::MainView>,
    /// Search panel asked the editor to select this byte range (find
    /// next / previous, or auto-jump as the user types a query).
    pub editor_jump: Option<Match>,
    /// Search panel asked the editor to splice replacements over these
    /// ranges. Each tuple is `(range, expected current text, new text)`.
    /// The list is ordered highest-offset first by the search panel so
    /// the shell can apply them in sequence without offset drift.
    pub editor_splices: Vec<(Match, String, String)>,
    /// Async task to run (e.g. the file picker future). `Task` doesn't
    /// implement `Default`/`Debug`, hence the manual construction below.
    pub task: Task<Message>,
}

impl Default for SidebarReaction {
    fn default() -> Self {
        Self {
            open: None,
            open_project: None,
            fs_changed: false,
            reimport_requested: false,
            show_view: None,
            editor_jump: None,
            editor_splices: Vec::new(),
            task: Task::none(),
        }
    }
}

impl std::fmt::Debug for SidebarReaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidebarReaction")
            .field("open", &self.open)
            .field("open_project", &self.open_project)
            .field("fs_changed", &self.fs_changed)
            .field("reimport_requested", &self.reimport_requested)
            .finish_non_exhaustive()
    }
}

impl Sidebar {
    pub(crate) const fn new() -> Self {
        Self {
            project_root: None,
            project_name: String::new(),
            groups: BTreeMap::new(),
            dialog: None,
            tab: Tab::Project,
            search: SearchState::new(),
        }
    }

    pub(crate) const fn tab(&self) -> Tab {
        self.tab
    }

    pub(crate) const fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
    }

    /// Used by Ctrl-F / Ctrl-H to push the Search panel into find-only
    /// or find-and-replace mode regardless of what mode it last held.
    pub(crate) const fn set_search_mode(&mut self, mode: search::Mode) {
        self.search.set_mode(mode);
    }

    pub(crate) fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
    }

    // Single match dispatch over Message variants; splitting it for the
    // sake of a clippy length lint hurts readability.
    //
    // `body` is the current editor buffer text. Only the Search sub-
    // panel needs it; the rest of the sidebar ignores it. Threading it
    // through `update` (rather than caching a copy) keeps the editor
    // as the single source of truth — we never act on stale prose.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn update(
        &mut self,
        message: Message,
        body: Option<&str>,
    ) -> SidebarReaction {
        match message {
            Message::PickProject => {
                let task = Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Pick a project folder")
                            .pick_folder()
                            .await
                            .map(|h| h.path().to_path_buf())
                    },
                    Message::ProjectPicked,
                );
                SidebarReaction { task, ..Default::default() }
            }
            Message::Reimport => SidebarReaction {
                reimport_requested: true,
                ..Default::default()
            },
            Message::ShowEditor => SidebarReaction {
                show_view: Some(crate::views::MainView::Editor),
                ..Default::default()
            },
            Message::ShowCharacters => SidebarReaction {
                show_view: Some(crate::views::MainView::Characters),
                ..Default::default()
            },
            Message::ShowLocations => SidebarReaction {
                show_view: Some(crate::views::MainView::Locations),
                ..Default::default()
            },
            Message::ShowCorkboard => SidebarReaction {
                show_view: Some(crate::views::MainView::Corkboard),
                ..Default::default()
            },
            Message::ShowTimeline => SidebarReaction {
                show_view: Some(crate::views::MainView::Timeline),
                ..Default::default()
            },
            Message::ShowRelationships => SidebarReaction {
                show_view: Some(crate::views::MainView::Relationships),
                ..Default::default()
            },
            Message::ShowResearch => SidebarReaction {
                show_view: Some(crate::views::MainView::Research),
                ..Default::default()
            },
            Message::ProjectPicked(None) => SidebarReaction::default(),
            Message::ProjectPicked(Some(path)) => {
                // The shell observes the pick via [`SidebarReaction::open_project`]
                // — we do NOT fire a follow-up message here because that would
                // bounce back through `Sidebar::update` and either no-op or
                // (worse) loop forever.
                SidebarReaction {
                    open_project: Some(path),
                    ..Default::default()
                }
            }
            Message::ProjectLoaded { root, name, scan } => {
                self.project_root = Some(root);
                self.project_name = name;
                self.groups.clear();
                for kind in DocumentKind::ALL {
                    self.groups.insert(
                        kind,
                        Group {
                            entries: Vec::new(),
                            expanded: matches!(
                                kind,
                                DocumentKind::Chapter | DocumentKind::Character
                            ),
                        },
                    );
                }
                for (kind, path) in scan {
                    let display_name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("(unnamed)")
                        .to_owned();
                    self.groups.entry(kind).or_default().entries.push(Entry {
                        abs_path: path,
                        display_name,
                    });
                }
                for group in self.groups.values_mut() {
                    group.entries.sort_by(|a, b| a.display_name.cmp(&b.display_name));
                }
                SidebarReaction::default()
            }
            Message::ToggleGroup(kind) => {
                if let Some(group) = self.groups.get_mut(&kind) {
                    group.expanded = !group.expanded;
                }
                SidebarReaction::default()
            }
            Message::Open(path) => {
                SidebarReaction { open: Some(path), ..Default::default() }
            }
            Message::NewDocumentPrompt(kind) => {
                self.dialog = Some(Dialog::CreateDocument { kind, draft: String::new() });
                SidebarReaction::default()
            }
            Message::NewDocumentDraftChanged(s) => {
                if let Some(Dialog::CreateDocument { draft, .. }) = self.dialog.as_mut() {
                    *draft = s;
                }
                SidebarReaction::default()
            }
            Message::NewDocumentSubmit => {
                let Some(Dialog::CreateDocument { kind, draft }) = self.dialog.take() else {
                    return SidebarReaction::default();
                };
                let trimmed = draft.trim().to_owned();
                let Some(root) = self.project_root.clone() else {
                    return SidebarReaction::default();
                };
                if trimmed.is_empty() {
                    return SidebarReaction::default();
                }
                let path = root.join(kind.folder()).join(format!("{trimmed}.md"));
                if let Err(err) = std::fs::create_dir_all(path.parent().unwrap()) {
                    tracing::error!(%err, "could not create folder");
                    return SidebarReaction::default();
                }
                if path.exists() {
                    tracing::warn!(path = %path.display(), "file already exists; aborting");
                    return SidebarReaction::default();
                }
                let stub = stub_for(kind, &trimmed);
                if let Err(err) = std::fs::write(&path, stub) {
                    tracing::error!(%err, "could not create document");
                    return SidebarReaction::default();
                }
                SidebarReaction {
                    open: Some(path),
                    fs_changed: true,
                    ..Default::default()
                }
            }
            Message::RenamePrompt(abs_path) => {
                let stem = abs_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_owned();
                self.dialog =
                    Some(Dialog::RenameDocument { abs_path, draft: stem });
                SidebarReaction::default()
            }
            Message::RenameDraftChanged(s) => {
                if let Some(Dialog::RenameDocument { draft, .. }) = self.dialog.as_mut() {
                    *draft = s;
                }
                SidebarReaction::default()
            }
            Message::RenameSubmit => {
                let Some(Dialog::RenameDocument { abs_path, draft }) = self.dialog.take() else {
                    return SidebarReaction::default();
                };
                let trimmed = draft.trim().to_owned();
                if trimmed.is_empty() {
                    return SidebarReaction::default();
                }
                let Some(parent) = abs_path.parent() else {
                    return SidebarReaction::default();
                };
                let new_path = parent.join(format!("{trimmed}.md"));
                if new_path == abs_path {
                    return SidebarReaction::default();
                }
                if new_path.exists() {
                    tracing::warn!(path = %new_path.display(), "rename target exists; aborting");
                    return SidebarReaction::default();
                }
                if let Err(err) = std::fs::rename(&abs_path, &new_path) {
                    tracing::error!(%err, "rename failed");
                    return SidebarReaction::default();
                }
                SidebarReaction {
                    open: Some(new_path),
                    fs_changed: true,
                    ..Default::default()
                }
            }
            Message::DeletePrompt(abs_path) => {
                let display = abs_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("(this file)")
                    .to_owned();
                self.dialog = Some(Dialog::ConfirmDelete { abs_path, display });
                SidebarReaction::default()
            }
            Message::DeleteConfirm => {
                let Some(Dialog::ConfirmDelete { abs_path, .. }) = self.dialog.take() else {
                    return SidebarReaction::default();
                };
                if let Err(err) = std::fs::remove_file(&abs_path) {
                    tracing::error!(%err, "delete failed");
                    return SidebarReaction::default();
                }
                SidebarReaction { fs_changed: true, ..Default::default() }
            }
            Message::DialogDismiss => {
                self.dialog = None;
                SidebarReaction::default()
            }
            Message::TabSelected(tab) => {
                self.tab = tab;
                SidebarReaction::default()
            }
            Message::Search(sub) => {
                let reaction = self.search.update(sub, body);
                SidebarReaction {
                    editor_jump: reaction.jump_to,
                    editor_splices: reaction.splices,
                    ..Default::default()
                }
            }
        }
    }

    pub(crate) fn view(&self, body: Option<&str>) -> Element<'_, Message> {
        // Persistent header: project title, open/reindex, main-view
        // switcher icons, sidebar tab strip. These stay visible no
        // matter which sidebar tab is active — the main-view icons
        // control the centre pane (orthogonal to the sidebar tab) and
        // the project controls are global.
        let header = column![
            row![
                text(if self.project_root.is_some() {
                    self.project_name.clone()
                } else {
                    "(no project)".to_owned()
                })
                .size(14),
            ]
            .padding([0, 8]),
            row![
                button(text("Open project…").size(12))
                    .on_press(Message::PickProject)
                    .width(Length::FillPortion(2)),
                button(text("Re-index").size(12))
                    .on_press(Message::Reimport)
                    .style(button::secondary)
                    .width(Length::FillPortion(1)),
            ]
            .spacing(4),
            row![
                view_icon("\u{270E}", "Editor", Message::ShowEditor),
                view_icon("\u{263B}", "Characters", Message::ShowCharacters),
                view_icon("\u{26EF}", "Locations", Message::ShowLocations),
                view_icon("\u{25A4}", "Scenes", Message::ShowCorkboard),
                view_icon("\u{21C4}", "Timeline", Message::ShowTimeline),
                view_icon("\u{232C}", "Graph", Message::ShowRelationships),
                view_icon("\u{273A}", "Research", Message::ShowResearch),
            ]
            .spacing(4),
            tab_strip(self.tab),
            rule::horizontal(1.0),
        ]
        .spacing(8)
        .padding(12)
        .width(Length::Fill);

        let tab_body: Element<'_, Message> = match self.tab {
            Tab::Project => self.project_tab_body(),
            Tab::Search => self.search.view(body).map(Message::Search),
        };

        scrollable(column![header, tab_body].width(Length::Fill))
            .height(Length::Fill)
            .width(Length::Fill)
            .into()
    }

    fn project_tab_body(&self) -> Element<'_, Message> {
        let mut col = column![].spacing(8).padding([0, 12]).width(Length::Fill);
        if let Some(dialog) = &self.dialog {
            col = col.push(dialog_view(dialog));
            col = col.push(rule::horizontal(1.0));
        }
        if self.project_root.is_some() {
            for kind in DocumentKind::ALL {
                let group = self.groups.get(&kind);
                let entries_len = group.map_or(0, |g| g.entries.len());
                let expanded = group.is_some_and(|g| g.expanded);
                let header_label = format!(
                    "{} {} ({entries_len})",
                    if expanded { "▾" } else { "▸" },
                    folder_label(kind),
                );
                col = col.push(
                    row![
                        button(text(header_label).size(12))
                            .on_press(Message::ToggleGroup(kind))
                            .width(Length::Fill)
                            .style(button::secondary),
                        button(text("+").size(12))
                            .on_press(Message::NewDocumentPrompt(kind))
                            .style(button::secondary),
                    ]
                    .spacing(4),
                );
                if expanded {
                    if let Some(group) = group {
                        for entry in &group.entries {
                            col = col.push(file_row(entry));
                        }
                    }
                }
            }
        }
        col.into()
    }
}

/// Two-button row to switch between the Project tree and the Search
/// panel. The active tab takes the primary button style so it reads as
/// "selected"; the inactive one stays secondary.
fn tab_strip(active: Tab) -> Element<'static, Message> {
    let project_style = if active == Tab::Project { button::primary } else { button::secondary };
    let search_style = if active == Tab::Search { button::primary } else { button::secondary };
    row![
        button(text("Project").size(12))
            .on_press(Message::TabSelected(Tab::Project))
            .style(project_style)
            .width(Length::FillPortion(1)),
        button(text("Search").size(12))
            .on_press(Message::TabSelected(Tab::Search))
            .style(search_style)
            .width(Length::FillPortion(1)),
    ]
    .spacing(4)
    .into()
}

/// View-switch icon button with a hover tooltip. Icons are plain Unicode
/// glyphs — no icon font to ship and they render on every system font
/// stack we'll realistically meet. Glyphs picked from the geometric and
/// dingbats blocks so they look at home next to text.
fn view_icon(
    glyph: &'static str,
    tooltip_text: &'static str,
    message: Message,
) -> Element<'static, Message> {
    let btn = button(text(glyph).size(15).center())
        .on_press(message)
        .style(button::secondary)
        .width(Length::FillPortion(1));
    tooltip(btn, text(tooltip_text).size(11), TooltipPosition::Bottom)
        .padding(4)
        .into()
}

fn file_row(entry: &Entry) -> Element<'static, Message> {
    let display = entry.display_name.clone();
    let path_open = entry.abs_path.clone();
    let path_rename = entry.abs_path.clone();
    let path_delete = entry.abs_path.clone();
    row![
        button(text(display).size(12))
            .on_press(Message::Open(path_open))
            .width(Length::Fill)
            .style(button::text),
        button(text("⋯").size(12))
            .on_press(Message::RenamePrompt(path_rename))
            .style(button::text),
        button(text("✕").size(12))
            .on_press(Message::DeletePrompt(path_delete))
            .style(button::text),
    ]
    .spacing(2)
    .padding([0, 8])
    .into()
}

fn dialog_view(dialog: &Dialog) -> Element<'_, Message> {
    match dialog {
        Dialog::ConfirmDelete { display, .. } => container(
            column![
                text(format!("Delete {display}?")).size(12),
                row![
                    button(text("Delete").size(12))
                        .on_press(Message::DeleteConfirm)
                        .style(button::danger),
                    button(text("Cancel").size(12))
                        .on_press(Message::DialogDismiss)
                        .style(button::secondary),
                ]
                .spacing(8),
            ]
            .spacing(6)
            .padding(8),
        )
        .width(Length::Fill)
        .into(),
        Dialog::CreateDocument { kind, draft } => container(
            column![
                text(format!("New {} document", folder_label(*kind))).size(12),
                text_input("Title", draft)
                    .on_input(Message::NewDocumentDraftChanged)
                    .on_submit(Message::NewDocumentSubmit)
                    .size(12),
                row![
                    button(text("Create").size(12))
                        .on_press(Message::NewDocumentSubmit)
                        .style(button::primary),
                    button(text("Cancel").size(12))
                        .on_press(Message::DialogDismiss)
                        .style(button::secondary),
                ]
                .spacing(8),
            ]
            .spacing(6)
            .padding(8),
        )
        .width(Length::Fill)
        .into(),
        Dialog::RenameDocument { draft, .. } => container(
            column![
                text("Rename to").size(12),
                text_input("Title", draft)
                    .on_input(Message::RenameDraftChanged)
                    .on_submit(Message::RenameSubmit)
                    .size(12),
                row![
                    button(text("Rename").size(12))
                        .on_press(Message::RenameSubmit)
                        .style(button::primary),
                    button(text("Cancel").size(12))
                        .on_press(Message::DialogDismiss)
                        .style(button::secondary),
                ]
                .spacing(8),
            ]
            .spacing(6)
            .padding(8),
        )
        .width(Length::Fill)
        .into(),
    }
}

const fn folder_label(kind: DocumentKind) -> &'static str {
    kind.folder()
}


/// Minimal Markdown stub for a freshly created document. Includes YAML
/// frontmatter so it round-trips cleanly through `letswrite_core::Document`.
fn stub_for(kind: DocumentKind, title: &str) -> String {
    let type_field = match kind {
        DocumentKind::Chapter => "chapter",
        DocumentKind::Scene => "scene",
        DocumentKind::Idea => "idea",
        DocumentKind::Character => "character",
        DocumentKind::Location => "location",
        DocumentKind::Meta => "meta",
        DocumentKind::Research => "research",
    };
    format!("---\ntitle: {title}\ntype: {type_field}\n---\n# {title}\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use letswrite_core::Project;
    use tempfile::tempdir;

    fn primed_sidebar(root: &Path) -> Sidebar {
        let project = Project::init(root, "T").unwrap();
        let scan = project
            .scan()
            .into_iter()
            .map(|f| (f.kind, f.path))
            .collect();
        let mut sb = Sidebar::new();
        let _ = sb.update(
            Message::ProjectLoaded {
                root: root.to_path_buf(),
                name: project.name().to_owned(),
                scan,
            },
            None,
        );
        sb
    }

    #[test]
    fn new_document_creates_file_with_frontmatter_and_marks_fs_changed() {
        let dir = tempdir().unwrap();
        let mut sb = primed_sidebar(dir.path());
        let _ = sb.update(Message::NewDocumentPrompt(DocumentKind::Chapter), None);
        let _ = sb.update(Message::NewDocumentDraftChanged("Chapter 3".to_owned()), None);
        let reaction = sb.update(Message::NewDocumentSubmit, None);
        assert!(reaction.fs_changed);
        let path = dir.path().join("Chapters/Chapter 3.md");
        assert!(path.is_file());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("---"));
        assert!(text.contains("type: chapter"));
        assert!(text.contains("# Chapter 3"));
        assert_eq!(reaction.open.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn rename_moves_file_and_returns_new_path() {
        let dir = tempdir().unwrap();
        let mut sb = primed_sidebar(dir.path());
        let original = dir.path().join("Chapters/Old.md");
        std::fs::write(&original, "body").unwrap();
        let _ = sb.update(Message::RenamePrompt(original.clone()), None);
        let _ = sb.update(Message::RenameDraftChanged("New Title".to_owned()), None);
        let reaction = sb.update(Message::RenameSubmit, None);
        assert!(reaction.fs_changed);
        assert!(!original.exists());
        let new_path = dir.path().join("Chapters/New Title.md");
        assert!(new_path.is_file());
        assert_eq!(reaction.open.as_deref(), Some(new_path.as_path()));
    }

    #[test]
    fn delete_requires_confirmation_and_removes_file() {
        let dir = tempdir().unwrap();
        let mut sb = primed_sidebar(dir.path());
        let path = dir.path().join("Ideas/Gone.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();

        // Prompting alone shouldn't touch the file.
        let _ = sb.update(Message::DeletePrompt(path.clone()), None);
        assert!(path.exists());

        let reaction = sb.update(Message::DeleteConfirm, None);
        assert!(reaction.fs_changed);
        assert!(!path.exists());
    }

    #[test]
    fn delete_dismiss_keeps_the_file() {
        let dir = tempdir().unwrap();
        let mut sb = primed_sidebar(dir.path());
        let path = dir.path().join("Ideas/Keep.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();
        let _ = sb.update(Message::DeletePrompt(path.clone()), None);
        let reaction = sb.update(Message::DialogDismiss, None);
        assert!(!reaction.fs_changed);
        assert!(path.exists());
    }

    #[test]
    fn rename_to_existing_file_aborts() {
        let dir = tempdir().unwrap();
        let mut sb = primed_sidebar(dir.path());
        let a = dir.path().join("Ideas/A.md");
        let b = dir.path().join("Ideas/B.md");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::write(&a, "1").unwrap();
        std::fs::write(&b, "2").unwrap();
        let _ = sb.update(Message::RenamePrompt(a.clone()), None);
        let _ = sb.update(Message::RenameDraftChanged("B".to_owned()), None);
        let reaction = sb.update(Message::RenameSubmit, None);
        assert!(!reaction.fs_changed, "should refuse to overwrite existing file");
        assert!(a.exists());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "2");
    }
}
