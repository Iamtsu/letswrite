//! Application root: state, messages, and the two-column shell layout.

// Scaffolding: parts of state are unused until later tasks land.
#![allow(clippy::unused_self, clippy::missing_const_for_fn)]

use std::path::PathBuf;

use iced::event::{self, Event};
use iced::keyboard::{self, Modifiers};
use iced::mouse;
use iced::widget::pane_grid::{self, Configuration, Node, PaneGrid, ResizeEvent};
use iced::widget::{container, text};
use iced::{Background, Border, Color, Element, Length, Subscription, Task, Theme};

use letswrite_core::settings::{ThemePreference, EDITOR_FONT_MAX, EDITOR_FONT_MIN};
use letswrite_core::{Project, Settings};
use letswrite_import::import_project;

use crate::editor::{self, Editor};
use crate::sidebar::{self, Sidebar};
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
    panes: pane_grid::State<Pane>,
    editor: Editor,
    sidebar: Sidebar,
    project: Option<Project>,
    /// Tracked here because Iced's `listen_with` filter is a plain `fn`
    /// pointer and can't see `App` state; we update this on
    /// `ModifiersChanged` events and read it on `WheelScrolled` to decide
    /// whether to act.
    modifiers: Modifiers,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    PaneResized(ResizeEvent),
    Editor(editor::Message),
    Sidebar(sidebar::Message),
    /// Cycle through the available syntax themes (until a settings UI lands).
    #[allow(dead_code)] // wired by a settings UI later (#11 / TBD)
    CycleSyntaxTheme,
    /// Global keyboard modifier state changed (Ctrl, Shift, Alt, …).
    ModifiersChanged(Modifiers),
    /// Mouse wheel scrolled by `delta` lines (positive = up). Only acted on
    /// when Ctrl is held in [`Self::ModifiersChanged`].
    WheelScrolled(f32),
}

impl App {
    pub(crate) fn new() -> (Self, Task<Message>) {
        let settings = Settings::load().unwrap_or_else(|err| {
            tracing::warn!(%err, "could not load settings; using defaults");
            Settings::default()
        });

        let panes = build_panes(&settings);
        let syntax_theme = SyntaxTheme::from_settings(settings.syntax_theme);
        let editor = Editor::new("(no document open)", syntax_theme, settings.window.editor_font_size);
        let sidebar = Sidebar::new();

        let mut app = Self {
            settings,
            panes,
            editor,
            sidebar,
            project: None,
            modifiers: Modifiers::default(),
        };

        // Auto-open the last project if it still exists. Each branch logs
        // and calls a different method, so map_or_else doesn't fit cleanly.
        #[allow(clippy::option_if_let_else)]
        let init_task = if let Some(last) = app.settings.last_project.clone() {
            if last.is_dir() {
                tracing::info!(path = %last.display(), "auto-opening last project");
                app.open_project(last)
            } else {
                tracing::warn!(path = %last.display(), "saved project no longer exists");
                Task::none()
            }
        } else {
            Task::none()
        };

        (app, init_task)
    }

    pub(crate) fn title(&self) -> String {
        self.project.as_ref().map_or_else(
            || "letswrite".to_owned(),
            |p| format!("{} — letswrite", p.name()),
        )
    }

    pub(crate) fn theme(&self) -> Theme {
        match self.settings.theme {
            ThemePreference::Dark | ThemePreference::System => Theme::Dark,
            ThemePreference::Light => Theme::Light,
        }
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        event::listen_with(global_event_filter)
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
            Message::Sidebar(msg) => self.handle_sidebar_message(msg),
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
            Message::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers;
                Task::none()
            }
            Message::WheelScrolled(delta) => {
                if !self.modifiers.control() || delta == 0.0 {
                    return Task::none();
                }
                let current = self.settings.window.editor_font_size;
                let step: i32 = if delta > 0.0 { 1 } else { -1 };
                let next = i32::from(current)
                    .saturating_add(step)
                    .clamp(i32::from(EDITOR_FONT_MIN), i32::from(EDITOR_FONT_MAX));
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let next_u16 = next as u16;
                if next_u16 == current {
                    return Task::none();
                }
                self.settings.window.editor_font_size = next_u16;
                self.editor.set_font_size(next_u16);
                tracing::debug!(font_size = next_u16, "editor font size changed");
                if let Err(err) = self.settings.save() {
                    tracing::warn!(%err, "could not persist editor font size");
                }
                Task::none()
            }
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let pane_grid = PaneGrid::new(&self.panes, |_id, pane, _is_maximized| {
            let body: Element<'_, Message> = match pane {
                Pane::Sidebar => container(self.sidebar.view().map(Message::Sidebar))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(pane_surface_style)
                    .into(),
                Pane::Editor => container(self.editor.view().map(Message::Editor))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(pane_surface_style)
                    .into(),
                Pane::Assistant => placeholder_pane("Assistant pane — coming soon"),
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

    fn handle_sidebar_message(&mut self, msg: sidebar::Message) -> Task<Message> {
        let reaction = self.sidebar.update(msg);
        let mut tasks: Vec<Task<Message>> = Vec::new();
        if let Some(root) = reaction.open_project {
            tasks.push(self.open_project(root));
        }
        if reaction.fs_changed {
            if let Some(project) = self.project.as_mut() {
                if let Err(err) = project.reindex() {
                    tracing::warn!(%err, "reindex after fs change failed");
                }
                tasks.push(self.refresh_sidebar());
            }
        }
        if reaction.reimport_requested {
            self.run_import();
        }
        if let Some(path) = reaction.open {
            if let Some(root) = self.sidebar.project_root() {
                let root = root.to_path_buf();
                tasks.push(Editor::open_path(root, path).map(Message::Editor));
            }
        }
        let sidebar_task = reaction.task.map(Message::Sidebar);
        tasks.push(sidebar_task);
        Task::batch(tasks)
    }

    fn open_project(&mut self, root: PathBuf) -> Task<Message> {
        match Project::open(&root) {
            Ok(mut project) => {
                if let Err(err) = project.reindex() {
                    tracing::warn!(%err, "initial reindex failed");
                }
                let name = project.name().to_owned();
                let scan = project
                    .scan()
                    .into_iter()
                    .map(|f| (f.kind, f.path))
                    .collect::<Vec<_>>();
                self.project = Some(project);
                self.settings.last_project = Some(root.clone());
                if let Err(err) = self.settings.save() {
                    tracing::warn!(%err, "could not persist last project");
                }
                // Run the importer on open so entities/scenes/mentions are
                // available immediately. Cheap for normal-sized projects;
                // moves to a background task only if we hit slowness later.
                self.run_import();
                Task::done(Message::Sidebar(sidebar::Message::ProjectLoaded {
                    root,
                    name,
                    scan,
                }))
            }
            Err(err) => {
                tracing::error!(%err, path = %root.display(), "could not open project");
                Task::none()
            }
        }
    }

    fn run_import(&mut self) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match import_project(project) {
            Ok(report) => tracing::info!(?report, "import completed"),
            Err(err) => tracing::error!(%err, "import failed"),
        }
    }

    fn refresh_sidebar(&self) -> Task<Message> {
        let Some(project) = self.project.as_ref() else {
            return Task::none();
        };
        let root = project.root().to_path_buf();
        let name = project.name().to_owned();
        let scan = project
            .scan()
            .into_iter()
            .map(|f| (f.kind, f.path))
            .collect::<Vec<_>>();
        Task::done(Message::Sidebar(sidebar::Message::ProjectLoaded {
            root,
            name,
            scan,
        }))
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

fn placeholder_pane(label: &'static str) -> Element<'static, Message> {
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

/// Global event filter for the Iced subscription. Iced expects a plain `fn`
/// pointer, so we surface raw events and let `update` consult `self.modifiers`
/// to decide what to do with wheel scrolls.
#[allow(clippy::needless_pass_by_value)]
fn global_event_filter(
    event: Event,
    _status: event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
            Some(Message::ModifiersChanged(m))
        }
        Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
            // Iced gives us either Lines{x, y} or Pixels{x, y}; we only care
            // about the y axis. Lines are usually ±1.0 per notch; pixels we
            // approximate with a scale.
            let y = match delta {
                mouse::ScrollDelta::Lines { y, .. } => y,
                mouse::ScrollDelta::Pixels { y, .. } => y / 15.0,
            };
            Some(Message::WheelScrolled(y))
        }
        _ => None,
    }
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
