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

// Note: `iced::widget::text_editor` is both a module (containing Style,
// Status, the `default` style fn, …) and a free-fn constructor. We import
// the module here and call the constructor as `text_editor::TextEditor::new`
// via the local alias `editor_widget` below.
use iced::widget::text_editor::{self, Action, Content, TextEditor};
use iced::widget::{button, column, container, markdown, row, scrollable, text};
use iced::{Border, Element, Font, Length, Task, Theme};

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
    /// Cached preview parse. Rebuilt only when the body actually changes,
    /// not every paint.
    preview_items: Vec<markdown::Item>,
    /// Hash of the body the cached items were parsed from. Cheap change
    /// detection that doesn't depend on a wall clock.
    preview_body_hash: u64,
}

/// How the editor pane is split between the raw Markdown and the rendered
/// preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewMode {
    /// Edit only — the original behaviour.
    Edit,
    /// Preview only — read-only rendered output.
    Preview,
    /// Editor on the left, preview on the right, sharing the pane.
    Split,
}

impl ViewMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Edit => "Edit",
            Self::Preview => "Preview",
            Self::Split => "Split",
        }
    }

    const ALL: [Self; 3] = [Self::Edit, Self::Split, Self::Preview];
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
    view_mode: ViewMode,
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
    /// User switched between edit / preview / split.
    SetViewMode(ViewMode),
    /// User clicked a link in the rendered preview.
    LinkClicked(markdown::Url),
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
            view_mode: ViewMode::Edit,
        }
    }

    pub(crate) const fn set_syntax_theme(&mut self, theme: SyntaxTheme) {
        self.syntax_theme = theme;
    }

    pub(crate) const fn set_font_size(&mut self, size: u16) {
        self.font_size = size;
    }

    // Exposed for future keyboard-shortcut handlers in the app shell.
    #[allow(dead_code)]
    pub(crate) const fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    #[allow(dead_code)]
    pub(crate) const fn set_view_mode(&mut self, mode: ViewMode) {
        self.view_mode = mode;
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
                    // Re-parse the preview lazily — only if the new body
                    // actually changed (catches `Action::Edit` no-ops and
                    // avoids reparsing on every keystroke when split mode
                    // isn't even visible — guard is on the hash check).
                    refresh_preview(open);
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
                let (preview_items, preview_body_hash) =
                    parse_preview(&document.body);
                let open = OpenDocument {
                    abs_path,
                    project_root,
                    rel_path: document.rel_path,
                    kind: document.kind,
                    frontmatter: document.frontmatter,
                    content,
                    last_edit: None,
                    is_dirty: false,
                    preview_items,
                    preview_body_hash,
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
            Message::SetViewMode(mode) => {
                self.view_mode = mode;
                Task::none()
            }
            Message::LinkClicked(url) => {
                tracing::info!(url = %url, "preview link clicked");
                // Wiki-links resolve to a custom scheme; real entity
                // navigation lands in #18/#22 alongside the character views.
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
        let snapshot = self.snapshot();
        let header = view_mode_toolbar(self.view_mode);
        let body = match self.view_mode {
            ViewMode::Edit => self.editor_view(open),
            ViewMode::Preview => self.preview_view(open),
            ViewMode::Split => row![
                container(self.editor_view(open))
                    .width(Length::FillPortion(1))
                    .height(Length::Fill),
                container(self.preview_view(open))
                    .width(Length::FillPortion(1))
                    .height(Length::Fill),
            ]
            .spacing(1)
            .into(),
        };
        let status = status_bar(snapshot);
        column![header, body, status].height(Length::Fill).into()
    }

    fn editor_view<'a>(&self, open: &'a OpenDocument) -> Element<'a, Message> {
        // Length::Shrink lets the editor grow to content height; we then
        // let `scrollable` provide the visible scrollbar. With Length::Fill
        // here, `text_editor` clamps to the parent and scrolls internally
        // without surfacing a bar — matching the Preview look needs the
        // outer scrollable.
        let editor = TextEditor::new(&open.content)
            .placeholder("Start writing…")
            .height(Length::Shrink)
            .padding(16)
            .font(Font::DEFAULT)
            .size(self.font_size)
            .on_action(Message::Action)
            .highlight_with::<MarkdownHighlighter>(
                syntax::Settings { theme: self.syntax_theme },
                format_highlight,
            )
            .style(editor_borderless_style);
        scrollable(editor)
            .height(Length::Fill)
            .width(Length::Fill)
            .into()
    }

    fn preview_view<'a>(&self, open: &'a OpenDocument) -> Element<'a, Message> {
        let settings = markdown::Settings::with_text_size(self.font_size);
        // Style is theme-derived; we don't have direct access to the active
        // iced::Theme here, so use the dark palette which matches our
        // default. A future settings panel could pipe the live theme in.
        let style = markdown::Style::from_palette(Theme::Dark.palette());
        let view = markdown::view(&open.preview_items, settings, style)
            .map(Message::LinkClicked);
        scrollable(container(view).padding(16).width(Length::Fill))
            .height(Length::Fill)
            .into()
    }
}

fn view_mode_toolbar(current: ViewMode) -> Element<'static, Message> {
    let buttons: Vec<Element<'static, Message>> = ViewMode::ALL
        .iter()
        .map(|&mode| {
            let label = text(mode.label()).size(12);
            let btn = button(label).on_press(Message::SetViewMode(mode));
            // Visual hint for the active mode — selecting a different style
            // variant. We use button.style to swap on the active one.
            let btn = if mode == current {
                btn.style(button::primary)
            } else {
                btn.style(button::secondary)
            };
            btn.into()
        })
        .collect();
    container(row(buttons).spacing(4).padding([4, 12]))
        .width(Length::Fill)
        .into()
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

/// Borderless variant of the default `text_editor` style. Removes the
/// rectangular outline so the editor blends with the pane background and
/// matches the look of the rendered preview (which has no border either).
/// All other style properties — background, value/selection color, focus
/// tint — come from the default.
fn editor_borderless_style(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let base = text_editor::default(theme, status);
    text_editor::Style {
        border: Border { width: 0.0, ..base.border },
        ..base
    }
}

/// Parse Markdown into the preview's cached `Vec<Item>`, returning the items
/// and a hash of the source body for change detection.
fn parse_preview(body: &str) -> (Vec<markdown::Item>, u64) {
    let rewritten = rewrite_wiki_links(body);
    let items: Vec<markdown::Item> = markdown::parse(&rewritten).collect();
    (items, hash_body(body))
}

/// Update `open.preview_items` if the body has changed since the last parse.
fn refresh_preview(open: &mut OpenDocument) {
    let body = open.content.text();
    let hash = hash_body(&body);
    if hash != open.preview_body_hash {
        let rewritten = rewrite_wiki_links(&body);
        open.preview_items = markdown::parse(&rewritten).collect();
        open.preview_body_hash = hash;
    }
}

fn hash_body(body: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut h);
    h.finish()
}

/// Rewrite Obsidian-style wiki-links `[[Name]]` into Markdown links pointing
/// at a `letswrite://entity/<Name>` URL. The URL is opaque to pulldown-cmark
/// and will round-trip back to us via [`Message::LinkClicked`]. Resolution
/// against the entity index lands with the importer (#8).
fn rewrite_wiki_links(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if i + 1 < n && bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(close) = find_wiki_close(src, i + 2) {
                let name = &src[i + 2..close];
                // Allow piped `[[Target|Display]]` (Obsidian convention).
                let (target, display) = name
                    .split_once('|')
                    .map_or((name, name), |(t, d)| (t.trim(), d.trim()));
                let encoded_target = url_encode_path_segment(target);
                out.push('[');
                out.push_str(display);
                out.push_str("](letswrite://entity/");
                out.push_str(&encoded_target);
                out.push(')');
                i = close + 2;
                continue;
            }
        }
        out.push(src[i..].chars().next().expect("byte index is char boundary"));
        i += src[i..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

fn find_wiki_close(s: &str, from: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = from;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b']' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Minimal URL-encoder for a single path segment. We only escape characters
/// pulldown-cmark would mis-parse inside a link destination — spaces and
/// the small set of always-reserved characters. Anything else (Unicode,
/// punctuation) passes through.
fn url_encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            _ => out.push(c),
        }
    }
    out
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
    fn wiki_links_rewrite_to_letswrite_urls() {
        let input = "Talked with [[Evan Calder]] this morning.";
        let out = rewrite_wiki_links(input);
        assert_eq!(
            out,
            "Talked with [Evan Calder](letswrite://entity/Evan%20Calder) this morning."
        );
    }

    #[test]
    fn wiki_links_support_piped_display() {
        let out = rewrite_wiki_links("see [[Evan Calder|Evan]] now");
        assert_eq!(
            out,
            "see [Evan](letswrite://entity/Evan%20Calder) now"
        );
    }

    #[test]
    fn wiki_links_unclosed_left_as_is() {
        // Mid-edit: writer just typed `[[`. Don't garble the prose.
        let out = rewrite_wiki_links("starting [[a link that isn't closed yet");
        assert_eq!(out, "starting [[a link that isn't closed yet");
    }

    #[test]
    fn wiki_links_inside_paragraphs_preserve_surrounding_text() {
        let out = rewrite_wiki_links("- bullet with [[Aletheia]]\nmore text\n");
        assert!(out.contains("[Aletheia](letswrite://entity/Aletheia)"));
        assert!(out.starts_with("- bullet"));
        assert!(out.ends_with("more text\n"));
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
