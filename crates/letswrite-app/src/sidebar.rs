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

use iced::widget::{button, column, container, horizontal_rule, row, scrollable, text, text_input};
use iced::{Element, Length, Task};

use letswrite_core::DocumentKind;

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
    /// Result of the file-picker future.
    ProjectPicked(Option<PathBuf>),
    /// The app shell finished opening a project and gives us the scan.
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
}

/// What the sidebar wants the app shell to do after handling a message.
pub(crate) struct SidebarReaction {
    /// File to open in the editor (after rename, this points at the new path).
    pub open: Option<PathBuf>,
    /// Filesystem mutated — app should reindex and re-scan.
    pub fs_changed: bool,
    /// Async task to run (e.g. the file picker future). `Task` doesn't
    /// implement `Default`/`Debug`, hence the manual construction below.
    pub task: Task<Message>,
}

impl Default for SidebarReaction {
    fn default() -> Self {
        Self { open: None, fs_changed: false, task: Task::none() }
    }
}

impl std::fmt::Debug for SidebarReaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidebarReaction")
            .field("open", &self.open)
            .field("fs_changed", &self.fs_changed)
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
        }
    }

    pub(crate) fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
    }

    // Single match dispatch over Message variants; splitting it for the
    // sake of a clippy length lint hurts readability.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn update(&mut self, message: Message) -> SidebarReaction {
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
            Message::ProjectPicked(None) => SidebarReaction::default(),
            Message::ProjectPicked(Some(path)) => {
                // App shell takes over: opens the project, scans, sends back
                // ProjectLoaded. The reaction carries no task, just a marker
                // that the shell should pick this up via the dedicated
                // pick-result message in app.rs.
                SidebarReaction {
                    task: Task::done(Message::ProjectLoaded {
                        root: path.clone(),
                        name: project_name_from_path(&path),
                        scan: Vec::new(), // populated by the shell after init
                    }),
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
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let mut col = column![
            row![
                text(if self.project_root.is_some() {
                    self.project_name.clone()
                } else {
                    "(no project)".to_owned()
                })
                .size(14),
            ]
            .padding([0, 8]),
            button(text("Open project…").size(12))
                .on_press(Message::PickProject)
                .width(Length::Fill),
            horizontal_rule(1),
        ]
        .spacing(8)
        .padding(12)
        .width(Length::Fill);

        if let Some(dialog) = &self.dialog {
            col = col.push(dialog_view(dialog));
            col = col.push(horizontal_rule(1));
        }

        if self.project_root.is_some() {
            for kind in DocumentKind::ALL {
                let group = self.groups.get(&kind);
                let entries_len = group.map_or(0, |g| g.entries.len());
                let expanded = group.is_some_and(|g| g.expanded);
                let header_label =
                    format!("{} {} ({entries_len})", if expanded { "▾" } else { "▸" }, folder_label(kind));
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

        scrollable(col).height(Length::Fill).width(Length::Fill).into()
    }
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

fn project_name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .map_or_else(|| path.display().to_string(), str::to_owned)
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
        let _ = sb.update(Message::ProjectLoaded {
            root: root.to_path_buf(),
            name: project.name().to_owned(),
            scan,
        });
        sb
    }

    #[test]
    fn new_document_creates_file_with_frontmatter_and_marks_fs_changed() {
        let dir = tempdir().unwrap();
        let mut sb = primed_sidebar(dir.path());
        let _ = sb.update(Message::NewDocumentPrompt(DocumentKind::Chapter));
        let _ = sb.update(Message::NewDocumentDraftChanged("Chapter 3".to_owned()));
        let reaction = sb.update(Message::NewDocumentSubmit);
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
        let _ = sb.update(Message::RenamePrompt(original.clone()));
        let _ = sb.update(Message::RenameDraftChanged("New Title".to_owned()));
        let reaction = sb.update(Message::RenameSubmit);
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
        let _ = sb.update(Message::DeletePrompt(path.clone()));
        assert!(path.exists());

        let reaction = sb.update(Message::DeleteConfirm);
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
        let _ = sb.update(Message::DeletePrompt(path.clone()));
        let reaction = sb.update(Message::DialogDismiss);
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
        let _ = sb.update(Message::RenamePrompt(a.clone()));
        let _ = sb.update(Message::RenameDraftChanged("B".to_owned()));
        let reaction = sb.update(Message::RenameSubmit);
        assert!(!reaction.fs_changed, "should refuse to overwrite existing file");
        assert!(a.exists());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "2");
    }
}
