//! Markdown editor pane.
//!
//! Wraps `iced::widget::text_editor` with project-aware loading and
//! debounced autosave. The editor only ever sees the document *body*;
//! frontmatter is held aside on [`OpenDocument`] and round-tripped at
//! save time so it survives intact even if not yet user-editable.
//!
//! The cursor position, selection range, and word count are exposed via
//! [`EditorSnapshot`] for the assistant column (#13/#14) and status bar.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::widget::text_editor::{Action, Content};
use iced::widget::{column, container, row, text, text_editor};
use iced::{Element, Font, Length, Task};

use letswrite_core::{Document, DocumentKind};
use serde_yaml::Value as YamlValue;
use unicode_segmentation::UnicodeSegmentation;

use crate::syntax::{self, MarkdownHighlighter, SyntaxTheme};

/// Time between the last keystroke and an autosave write.
const AUTOSAVE_IDLE: Duration = Duration::from_millis(500);

/// One file open in the editor.
#[derive(Debug)]
pub(crate) struct OpenDocument {
    /// Absolute path on disk. Used by #7 navigation (window title, recent
    /// files) and watcher reconciliation — kept here so it's the single
    /// source of truth across the editor's lifetime.
    #[allow(dead_code)]
    abs_path: PathBuf,
    /// Project root the document belongs to. Saving uses this to resolve
    /// the relative path and stay within the project boundary.
    project_root: PathBuf,
    rel_path: String,
    kind: Option<DocumentKind>,
    /// Frontmatter parsed from the file. Round-tripped on save; not visible
    /// in the editor yet (a structured editor lands later).
    frontmatter: YamlValue,
    /// The text buffer Iced is editing.
    content: Content,
    /// When did the buffer last change? `None` means no unsaved edits.
    last_edit: Option<Instant>,
    /// When did we last write to disk? Drives the "modified" indicator.
    is_dirty: bool,
}

/// Read-only view of the editor's state. Cheap to clone — used by the
/// assistant column and status bar to react to cursor/selection moves.
///
/// `kind` and `selection` are consumed by the AI context builder (#14);
/// the status bar only reads the path/cursor/word count fields today.
#[derive(Debug, Clone, Default)]
pub(crate) struct EditorSnapshot {
    pub rel_path: Option<String>,
    #[allow(dead_code)]
    pub kind: Option<DocumentKind>,
    pub cursor_line: usize,
    pub cursor_column: usize,
    #[allow(dead_code)]
    pub selection: Option<String>,
    pub word_count: usize,
    pub is_dirty: bool,
}

#[derive(Debug)]
pub(crate) struct Editor {
    open: Option<OpenDocument>,
    placeholder: String,
    syntax_theme: SyntaxTheme,
    font_size: u16,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    /// User performed an editor action (typed, moved cursor, selected, …).
    Action(Action),
    /// A file finished loading.
    Loaded(Result<LoadedFile, String>),
    /// Autosave timer ticked — write to disk if still idle.
    AutosaveTick,
    /// Background save completed.
    Saved(Result<(), String>),
}

#[derive(Debug, Clone)]
pub(crate) struct LoadedFile {
    pub abs_path: PathBuf,
    pub project_root: PathBuf,
    pub document: Document,
}

impl Editor {
    pub(crate) fn new(
        placeholder: impl Into<String>,
        syntax_theme: SyntaxTheme,
        font_size: u16,
    ) -> Self {
        Self {
            open: None,
            placeholder: placeholder.into(),
            syntax_theme,
            font_size,
        }
    }

    pub(crate) const fn set_syntax_theme(&mut self, theme: SyntaxTheme) {
        self.syntax_theme = theme;
    }

    pub(crate) const fn set_font_size(&mut self, size: u16) {
        self.font_size = size;
    }

    /// Schedule a file load. The result comes back as `Message::Loaded`.
    pub(crate) fn open_path(
        project_root: PathBuf,
        abs_path: PathBuf,
    ) -> Task<Message> {
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || Document::load(&project_root, &abs_path)
                    .map(|d| LoadedFile { abs_path, project_root, document: d }))
                    .await
                    .map_err(|e| format!("join error: {e}"))
                    .and_then(|res| res.map_err(|e| e.to_string()))
            },
            Message::Loaded,
        )
    }

    pub(crate) fn snapshot(&self) -> EditorSnapshot {
        let Some(open) = &self.open else {
            return EditorSnapshot::default();
        };
        let (line, column) = open.content.cursor_position();
        let selection = open.content.selection();
        let body = open.content.text();
        EditorSnapshot {
            rel_path: Some(open.rel_path.clone()),
            kind: open.kind,
            cursor_line: line,
            cursor_column: column,
            selection,
            word_count: count_words(&body),
            is_dirty: open.is_dirty,
        }
    }

    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Action(action) => {
                let Some(open) = self.open.as_mut() else {
                    return Task::none();
                };
                let is_edit = action.is_edit();
                open.content.perform(action);
                if is_edit {
                    open.last_edit = Some(Instant::now());
                    open.is_dirty = true;
                    // Schedule an autosave probe AUTOSAVE_IDLE from now.
                    // If the user keeps typing, last_edit moves forward and
                    // the probe will reschedule itself.
                    return Task::perform(
                        async {
                            tokio::time::sleep(AUTOSAVE_IDLE).await;
                        },
                        |()| Message::AutosaveTick,
                    );
                }
                Task::none()
            }
            Message::Loaded(Ok(loaded)) => {
                let LoadedFile { abs_path, project_root, document } = loaded;
                let content = Content::with_text(&document.body);
                let open = OpenDocument {
                    abs_path,
                    project_root,
                    rel_path: document.rel_path,
                    kind: document.kind,
                    frontmatter: document.frontmatter,
                    content,
                    last_edit: None,
                    is_dirty: false,
                };
                tracing::info!(
                    rel_path = %open.rel_path,
                    kind = ?open.kind,
                    "document loaded"
                );
                self.open = Some(open);
                Task::none()
            }
            Message::Loaded(Err(err)) => {
                tracing::error!(%err, "could not load document");
                Task::none()
            }
            Message::AutosaveTick => {
                let Some(open) = self.open.as_mut() else {
                    return Task::none();
                };
                let Some(last) = open.last_edit else {
                    return Task::none();
                };
                // If the user typed again after this probe was scheduled,
                // the still-pending probe will fire and re-check; skip now.
                if last.elapsed() < AUTOSAVE_IDLE {
                    return Task::none();
                }
                // Build a Document from the current buffer + the held-aside
                // frontmatter, then write off-thread so we don't block UI.
                let doc = Document {
                    rel_path: open.rel_path.clone(),
                    kind: open.kind,
                    title: derive_title(&open.frontmatter, &open.rel_path),
                    frontmatter: open.frontmatter.clone(),
                    body: open.content.text(),
                };
                let project_root = open.project_root.clone();
                open.last_edit = None; // claim this save attempt
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || doc.save(&project_root))
                            .await
                            .map_err(|e| format!("join error: {e}"))
                            .and_then(|res| res.map_err(|e| e.to_string()))
                    },
                    Message::Saved,
                )
            }
            Message::Saved(Ok(())) => {
                if let Some(open) = self.open.as_mut() {
                    open.is_dirty = false;
                    tracing::debug!(rel_path = %open.rel_path, "autosaved");
                }
                Task::none()
            }
            Message::Saved(Err(err)) => {
                tracing::error!(%err, "autosave failed");
                Task::none()
            }
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let Some(open) = &self.open else {
            return container(text(self.placeholder.clone()).size(13))
                .padding(16)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        };
        let editor = text_editor(&open.content)
            .placeholder("Start writing…")
            .height(Length::Fill)
            .padding(16)
            .font(Font::MONOSPACE)
            .size(self.font_size)
            .on_action(Message::Action)
            .highlight_with::<MarkdownHighlighter>(
                syntax::Settings { theme: self.syntax_theme },
                format_highlight,
            );

        let snapshot = self.snapshot();
        let status = status_bar(snapshot);

        column![editor, status].height(Length::Fill).into()
    }
}

fn status_bar(snapshot: EditorSnapshot) -> Element<'static, Message> {
    let path = snapshot.rel_path.unwrap_or_else(|| "(no file)".to_owned());
    let dirty_marker = if snapshot.is_dirty { "●" } else { " " };
    let cursor =
        format!("Ln {}, Col {}", snapshot.cursor_line + 1, snapshot.cursor_column + 1);
    let words = format!("{} words", snapshot.word_count);
    container(
        row![
            text(format!("{dirty_marker} {path}")).size(12),
            container(text("")).width(Length::Fill),
            text(cursor).size(12),
            text(words).size(12),
        ]
        .padding([4, 12])
        .spacing(16),
    )
    .into()
}

/// Unicode-aware word count: counts grapheme-based word tokens, so
/// `naïve`, Japanese, etc. all behave correctly.
fn count_words(text: &str) -> usize {
    text.unicode_words().count()
}

/// Plain fn pointer passed to `text_editor.highlight_with`. Iced requires
/// `fn(...)` here, not `Fn(...)`, so we receive the theme via the per-span
/// highlight payload (see [`syntax::Highlight`]).
#[allow(clippy::trivially_copy_pass_by_ref)] // signature dictated by Iced's fn pointer
fn format_highlight(
    highlight: &syntax::Highlight,
    _theme: &iced::Theme,
) -> iced::advanced::text::highlighter::Format<Font> {
    let (kind, theme) = *highlight;
    theme.format_for(kind)
}

fn derive_title(frontmatter: &YamlValue, rel_path: &str) -> String {
    if let YamlValue::Mapping(m) = frontmatter {
        if let Some(YamlValue::String(s)) = m.get(YamlValue::String("title".to_owned())) {
            return s.clone();
        }
    }
    std::path::Path::new(rel_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map_or_else(|| rel_path.to_owned(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_count_handles_unicode_segmentation() {
        assert_eq!(count_words("one two three"), 3);
        assert_eq!(count_words("naïve résumé café"), 3);
        assert_eq!(count_words("don't can't won't"), 3);
        assert_eq!(count_words(""), 0);
        assert_eq!(count_words("   "), 0);
    }

    #[test]
    fn snapshot_is_default_when_no_file_open() {
        let editor = Editor::new("placeholder", SyntaxTheme::default(), 15);
        let snap = editor.snapshot();
        assert!(snap.rel_path.is_none());
        assert!(!snap.is_dirty);
        assert_eq!(snap.word_count, 0);
    }
}
