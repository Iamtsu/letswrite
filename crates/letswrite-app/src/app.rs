//! Application root: state, messages, and the two-column shell layout.

// Scaffolding: parts of state are unused until #7 lands.
#![allow(clippy::unused_self, clippy::missing_const_for_fn)]

use std::path::PathBuf;

use iced::widget::pane_grid::{self, Configuration, Node, PaneGrid, ResizeEvent};
use iced::widget::{button, column, container, horizontal_rule, text};
use iced::{Background, Border, Color, Element, Length, Task, Theme};

use letswrite_core::settings::ThemePreference;
use letswrite_core::{I18n, Settings};

use crate::editor::{self, Editor};
use crate::syntax::SyntaxTheme;

#[derive(Debug, Clone, Copy)]
enum Pane {
    Sidebar,
    Editor,
    Assistant,
}

#[derive(Debug)]
pub(crate) struct App {
    settings: Settings,
    i18n: I18n,
    panes: pane_grid::State<Pane>,
    editor: Editor,
    project_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    PaneResized(ResizeEvent),
    Editor(editor::Message),
    /// Temporary: open the dogfood chapter from The-Threshold if it exists.
    /// Replaced by a real file picker in #7.
    OpenDogfoodChapter,
    /// Cycle through the available syntax themes (until a settings UI lands).
    CycleSyntaxTheme,
}

impl App {
    pub(crate) fn new() -> (Self, Task<Message>) {
        let settings = Settings::load().unwrap_or_else(|err| {
            tracing::warn!(%err, "could not load settings; using defaults");
            Settings::default()
        });
        let i18n = I18n::with_language(settings.ui_language.clone()).unwrap_or_else(|err| {
            tracing::error!(%err, "could not initialize i18n; UI will show key markers");
            I18n::with_language("en".parse().expect("en is valid"))
                .expect("english bundle should always parse")
        });
        tracing::info!(language = %i18n.current(), "i18n ready");

        let panes = build_panes(&settings);
        let editor_placeholder = i18n.tr("editor-placeholder");
        let syntax_theme = SyntaxTheme::from_settings(settings.syntax_theme);
        let editor = Editor::new(editor_placeholder, syntax_theme);

        (
            Self { settings, i18n, panes, editor, project_root: None },
            Task::none(),
        )
    }

    pub(crate) fn title(&self) -> String {
        self.i18n.tr("app-title")
    }

    pub(crate) fn theme(&self) -> Theme {
        match self.settings.theme {
            ThemePreference::Dark | ThemePreference::System => Theme::Dark,
            ThemePreference::Light => Theme::Light,
        }
    }

    // Iced's update contract takes the message by value; clippy's
    // needless_pass_by_value lint doesn't help us here.
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PaneResized(ResizeEvent { split, ratio }) => {
                self.panes.resize(split, ratio);
                self.persist_ratios_from_layout();
                Task::none()
            }
            Message::Editor(msg) => self.editor.update(msg).map(Message::Editor),
            Message::OpenDogfoodChapter => {
                let project_root =
                    PathBuf::from("/home/tsu/Projects/private/The-Threshold");
                let abs_path = project_root
                    .join("Chapters")
                    .join("Chapter 2")
                    .join("Chapter 2 - The Ghost File.md");
                if !abs_path.exists() {
                    tracing::warn!(path = %abs_path.display(), "dogfood file missing");
                    return Task::none();
                }
                self.project_root = Some(project_root.clone());
                Editor::open_path(project_root, abs_path).map(Message::Editor)
            }
            Message::CycleSyntaxTheme => {
                let next = next_syntax_theme(self.settings.syntax_theme);
                self.settings.syntax_theme = next;
                self.editor.set_syntax_theme(SyntaxTheme::from_settings(next));
                tracing::info!(theme = ?next, "syntax theme changed");
                if let Err(err) = self.settings.save() {
                    tracing::warn!(%err, "could not persist syntax theme");
                }
                Task::none()
            }
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let sidebar_heading = self.i18n.tr("sidebar-project-heading");
        let sidebar_empty = self.i18n.tr("sidebar-no-project");
        let assistant_label = self.i18n.tr("assistant-placeholder");

        let pane_grid = PaneGrid::new(&self.panes, move |_id, pane, _is_maximized| {
            let body: Element<'_, Message> = match pane {
                Pane::Sidebar => sidebar_view(sidebar_heading.clone(), sidebar_empty.clone()),
                Pane::Editor => container(self.editor.view().map(Message::Editor))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(pane_surface_style)
                    .into(),
                Pane::Assistant => placeholder_pane(assistant_label.clone()),
            };
            pane_grid::Content::new(body)
        })
        .on_resize(SPLITTER_GRAB_LEEWAY, Message::PaneResized)
        .spacing(SPLITTER_THICKNESS)
        .style(splitter_highlight_style);

        container(pane_grid)
            .padding(0)
            .style(splitter_background_style)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Walk the pane tree and write the two split ratios into settings.
    /// Layout shape is fixed (sidebar | (editor | assistant)) so the outer
    /// split is the sidebar ratio and the inner split is the editor ratio.
    fn persist_ratios_from_layout(&mut self) {
        let (sidebar, editor) = ratios_from_node(self.panes.layout());
        if let Some(r) = sidebar {
            self.settings.window.sidebar_ratio = r;
        }
        if let Some(r) = editor {
            self.settings.window.editor_ratio = r;
        }
        if let Err(err) = self.settings.save() {
            tracing::warn!(%err, "could not persist window layout");
        }
    }
}

fn build_panes(settings: &Settings) -> pane_grid::State<Pane> {
    let sidebar_ratio = settings.window.sidebar_ratio.clamp(0.05, 0.5);
    let editor_ratio = settings.window.editor_ratio.clamp(0.2, 0.9);
    pane_grid::State::with_configuration(Configuration::Split {
        axis: pane_grid::Axis::Vertical,
        ratio: sidebar_ratio,
        a: Box::new(Configuration::Pane(Pane::Sidebar)),
        b: Box::new(Configuration::Split {
            axis: pane_grid::Axis::Vertical,
            ratio: editor_ratio,
            a: Box::new(Configuration::Pane(Pane::Editor)),
            b: Box::new(Configuration::Pane(Pane::Assistant)),
        }),
    })
}

/// Extract `(outer_split_ratio, inner_split_ratio)` from our fixed
/// `sidebar | (editor | assistant)` layout.
fn ratios_from_node(node: &Node) -> (Option<f32>, Option<f32>) {
    if let Node::Split { ratio, b, .. } = node {
        let outer = Some(*ratio);
        let inner = if let Node::Split { ratio, .. } = b.as_ref() {
            Some(*ratio)
        } else {
            None
        };
        (outer, inner)
    } else {
        (None, None)
    }
}

fn sidebar_view(heading: String, empty_label: String) -> Element<'static, Message> {
    container(
        column![
            text(heading).size(14),
            horizontal_rule(1),
            text(empty_label).size(12),
            // Temporary dogfood entry — replaced by real navigation in #7.
            button(text("Open The Threshold / Chapter 2").size(12))
                .on_press(Message::OpenDogfoodChapter),
            horizontal_rule(1),
            button(text("Cycle syntax theme").size(12))
                .on_press(Message::CycleSyntaxTheme),
        ]
        .spacing(8)
        .padding(12),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(pane_surface_style)
    .into()
}

const fn next_syntax_theme(
    current: letswrite_core::settings::SyntaxTheme,
) -> letswrite_core::settings::SyntaxTheme {
    use letswrite_core::settings::SyntaxTheme as T;
    match current {
        T::ColorblindSafe => T::Solarized,
        T::Solarized => T::HighContrast,
        T::HighContrast => T::ColorblindSafe,
    }
}

fn placeholder_pane(label: String) -> Element<'static, Message> {
    container(text(label).size(13))
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(pane_surface_style)
        .into()
}

/// Width of the splitter line at rest, in pixels.
const SPLITTER_THICKNESS: u16 = 4;
/// Extra pixels on each side of the splitter where the resize cursor is active.
/// A thicker grab zone makes the drag affordance more forgiving than the
/// visible line suggests.
const SPLITTER_GRAB_LEEWAY: u16 = 6;

/// Background that shows through `PaneGrid`'s spacing gaps. Picked from the
/// theme palette so it works in both dark and light modes.
fn splitter_background_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.strong.color)),
        ..container::Style::default()
    }
}

/// Highlight a splitter when the user hovers or grabs it. Slightly brighter
/// than the resting splitter color, with a clear accent on pick.
fn splitter_highlight_style(theme: &Theme) -> pane_grid::Style {
    let palette = theme.extended_palette();
    pane_grid::Style {
        hovered_region: pane_grid::Highlight {
            background: Background::Color(Color {
                a: 0.25,
                ..palette.primary.base.color
            }),
            border: Border {
                width: 1.0,
                color: palette.primary.strong.color,
                radius: 0.0.into(),
            },
        },
        hovered_split: pane_grid::Line {
            color: palette.primary.base.color,
            width: f32::from(SPLITTER_THICKNESS),
        },
        picked_split: pane_grid::Line {
            color: palette.primary.strong.color,
            width: f32::from(SPLITTER_THICKNESS),
        },
    }
}

/// Opaque pane background. Without this, panes inherit transparency and the
/// splitter-gap color bleeds across the whole window.
fn pane_surface_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.base.color)),
        text_color: Some(palette.background.base.text),
        ..container::Style::default()
    }
}
