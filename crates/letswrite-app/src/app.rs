//! Application root: state, messages, and the two-column shell layout.

// Scaffolding: parts of state are unused until later tasks land.
#![allow(clippy::unused_self, clippy::missing_const_for_fn)]

use std::path::PathBuf;
use std::sync::Arc;

use iced::event::{self, Event};
use iced::keyboard::{self, Modifiers};
use iced::mouse;
use iced::widget::pane_grid::{self, Configuration, Node, PaneGrid, ResizeEvent};
use iced::widget::container;
use iced::{Background, Border, Color, Element, Length, Subscription, Task, Theme};

use letswrite_ai::{
    Agent, AnthropicProvider, AssistantContext, CredentialStore, DefaultAgent,
    KeyringCredentialStore, Provider,
};
use letswrite_core::settings::{ThemePreference, EDITOR_FONT_MAX, EDITOR_FONT_MIN};
use letswrite_core::{Project, Settings};
use letswrite_import::import_project;

use crate::assistant::{self, Assistant};
use crate::context_builder::{self, BuildInputs};
use crate::editor::{self, Editor};
use crate::search;
use crate::sidebar::{self, Sidebar};
use crate::syntax::SyntaxTheme;
use crate::views::characters::{self as characters_view, CharactersView};
use crate::views::corkboard::{self as corkboard_view, CorkboardView};
use crate::views::locations::{self as locations_view, LocationsView};
use crate::views::relationships::{self as relationships_view, RelationshipsView};
use crate::views::research::{self as research_view, ResearchView};
use crate::views::timeline::{self as timeline_view, TimelineView};
use crate::views::MainView;


const KEYRING_SERVICE: &str = "letswrite";
const ANTHROPIC_API_KEY: &str = "anthropic-api-key";

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
    assistant: Assistant,
    project: Option<Project>,
    credentials: Arc<dyn CredentialStore>,
    main_view: MainView,
    characters_view: CharactersView,
    locations_view: LocationsView,
    corkboard_view: CorkboardView,
    timeline_view: TimelineView,
    relationships_view: RelationshipsView,
    research_view: ResearchView,
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
    Assistant(assistant::Message),
    CharactersView(characters_view::Message),
    LocationsView(locations_view::Message),
    CorkboardView(corkboard_view::Message),
    TimelineView(timeline_view::Message),
    RelationshipsView(relationships_view::Message),
    ResearchView(research_view::Message),
    /// Cycle through the available syntax themes (until a settings UI lands).
    #[allow(dead_code)] // wired by a settings UI later (#11 / TBD)
    CycleSyntaxTheme,
    /// Global keyboard modifier state changed (Ctrl, Shift, Alt, …).
    ModifiersChanged(Modifiers),
    /// Mouse wheel scrolled by `delta` lines (positive = up). Only acted on
    /// when Ctrl is held in [`Self::ModifiersChanged`].
    WheelScrolled(f32),
    /// Ctrl-F / Ctrl-H pressed — open the sidebar's Search tab in the
    /// requested mode.
    OpenSearchPanel(search::Mode),
    /// Esc pressed while the Search tab is active — fall back to the
    /// Project tab so the file tree is visible again.
    CloseSearchPanel,
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

        // Credential store + agent. If the key isn't set yet, the
        // Assistant renders an inline "enter API key" prompt.
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(KeyringCredentialStore::new(KEYRING_SERVICE));
        let needs_api_key = !key_present(&*credentials);
        let agent = build_agent(Arc::clone(&credentials));
        let assistant = Assistant::new(agent, needs_api_key);

        let mut app = Self {
            settings,
            panes,
            editor,
            sidebar,
            assistant,
            project: None,
            credentials,
            main_view: MainView::Editor,
            characters_view: CharactersView::new(),
            locations_view: LocationsView::new(),
            corkboard_view: CorkboardView::new(),
            timeline_view: TimelineView::new(),
            relationships_view: RelationshipsView::new(),
            research_view: ResearchView::new(),
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
            Message::Editor(msg) => {
                // Re-detect mentions after a save (the body changed) AND
                // after a fresh load (offsets in the DB may be stale from
                // a previous session, or a sibling tool edited the file).
                // Either way, the suggestion list is only as good as the
                // most recent scan against the current body.
                let is_save = matches!(msg, editor::Message::Saved(Ok(())));
                let is_load = matches!(msg, editor::Message::Loaded(Ok(_)));
                let task = self.editor.update(msg).map(Message::Editor);
                if is_save || is_load {
                    self.run_mention_detection();
                }
                self.refresh_entities_in_scene();
                task
            }
            Message::Sidebar(msg) => self.handle_sidebar_message(msg),
            Message::Assistant(msg) => self.handle_assistant_message(msg),
            Message::CharactersView(msg) => self.handle_characters_view_message(msg),
            Message::LocationsView(msg) => self.handle_locations_view_message(msg),
            Message::CorkboardView(msg) => self.handle_corkboard_view_message(msg),
            Message::TimelineView(msg) => self.handle_timeline_view_message(msg),
            Message::RelationshipsView(msg) => self.handle_relationships_view_message(msg),
            Message::ResearchView(msg) => self.handle_research_view_message(msg),
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
            Message::OpenSearchPanel(mode) => {
                // Switch the sidebar to the Search tab and prime its
                // mode. The text_input inside the Search panel is what
                // we want focused, but iced 0.14's text_input lacks a
                // stable `focus(id)` helper that works through Element
                // mapping — focusing the input is best-effort and may
                // require a click in the field. We can revisit if it
                // proves annoying.
                self.sidebar.set_tab(sidebar::Tab::Search);
                self.sidebar.set_search_mode(mode);
                Task::none()
            }
            Message::CloseSearchPanel => {
                // Only swap back if we're actually showing Search;
                // otherwise Esc would still surface as a no-op message
                // but shouldn't change the user's place.
                if self.sidebar.tab() == sidebar::Tab::Search {
                    self.sidebar.set_tab(sidebar::Tab::Project);
                }
                Task::none()
            }
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let pane_grid = PaneGrid::new(&self.panes, |_id, pane, _is_maximized| {
            let body: Element<'_, Message> = match pane {
                Pane::Sidebar => {
                    let editor_body = self.editor.body();
                    container(
                        self.sidebar
                            .view(editor_body.as_deref())
                            .map(Message::Sidebar),
                    )
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(pane_surface_style)
                    .into()
                }
                Pane::Editor => {
                    let body: Element<'_, Message> = match self.main_view {
                        MainView::Editor => self.editor.view().map(Message::Editor),
                        MainView::Characters => {
                            self.characters_view.view().map(Message::CharactersView)
                        }
                        MainView::Locations => {
                            self.locations_view.view().map(Message::LocationsView)
                        }
                        MainView::Corkboard => {
                            self.corkboard_view.view().map(Message::CorkboardView)
                        }
                        MainView::Timeline => {
                            self.timeline_view.view().map(Message::TimelineView)
                        }
                        MainView::Relationships => self
                            .relationships_view
                            .view()
                            .map(Message::RelationshipsView),
                        MainView::Research => {
                            self.research_view.view().map(Message::ResearchView)
                        }
                    };
                    container(body)
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .style(pane_surface_style)
                        .into()
                }
                Pane::Assistant => container(self.assistant.view().map(Message::Assistant))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(pane_surface_style)
                    .into(),
            };
            pane_grid::Content::new(body)
        })
        .on_resize(f32::from(SPLITTER_GRAB_LEEWAY), Message::PaneResized)
        .spacing(f32::from(SPLITTER_THICKNESS))
        .style(splitter_highlight_style);

        container(pane_grid)
            .padding(0)
            .style(splitter_background_style)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn handle_sidebar_message(&mut self, msg: sidebar::Message) -> Task<Message> {
        let editor_body = self.editor.body();
        let reaction = self.sidebar.update(msg, editor_body.as_deref());
        let mut tasks: Vec<Task<Message>> = Vec::new();
        // Splices must apply before the jump — the jump's offsets are
        // computed against the post-splice buffer (e.g. "advance to the
        // next match" after a single Replace). The sidebar already
        // orders splices highest-offset-first so applying them in
        // sequence doesn't disturb earlier offsets.
        for (range, expected, replacement) in reaction.editor_splices {
            tasks.push(
                self.editor
                    .splice_at(range.start, range.end, &expected, replacement)
                    .map(Message::Editor),
            );
        }
        if let Some(span) = reaction.editor_jump {
            // Sidebar-driven jumps come from an explicit navigation —
            // Enter / Next / Prev / Replace — so focusing the editor is
            // the right behaviour: the user is asking to go to the
            // match and the yellow selection only paints when focused
            // (text_editor.rs:1021). The QueryChanged keystroke path
            // never emits a jump, so live typing stays in the input.
            tasks.push(
                self.editor
                    .jump_to_range(span.start, span.end)
                    .map(Message::Editor),
            );
        }
        if let Some(view) = reaction.show_view {
            self.main_view = view;
            if let Some(p) = self.project.as_ref() {
                match view {
                    MainView::Characters => self.characters_view.refresh_cards(p),
                    MainView::Locations => self.locations_view.refresh_cards(p),
                    MainView::Corkboard => {
                        if let Some(root) = self.sidebar.project_root() {
                            self.corkboard_view.refresh(p, root);
                        }
                    }
                    MainView::Timeline => self.timeline_view.refresh(p),
                    MainView::Relationships => self.relationships_view.refresh(p),
                    MainView::Research => self.research_view.refresh_cards(p),
                    MainView::Editor => {}
                }
            }
        }
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
                self.refresh_entities_in_scene();
                self.refresh_suggestions();
                if let Some(p) = self.project.as_ref() {
                    self.characters_view.refresh_cards(p);
                    self.locations_view.refresh_cards(p);
                    if let Some(root) = self.sidebar.project_root() {
                        self.corkboard_view.refresh(p, root);
                    }
                    self.timeline_view.refresh(p);
                    self.relationships_view.refresh(p);
                    self.research_view.refresh_cards(p);
                }
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

    fn run_mention_detection(&mut self) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let Some(rel) = self.editor.snapshot().rel_path else {
            return;
        };
        let Some(root) = self.sidebar.project_root() else {
            return;
        };
        let abs = root.join(rel);
        match letswrite_import::detect_for_document(project, &abs) {
            Ok(n) => tracing::debug!(suggestions = n, "mention detection ran"),
            Err(err) => tracing::warn!(%err, "mention detection failed"),
        }
        self.refresh_suggestions();
    }

    fn handle_characters_view_message(
        &mut self,
        msg: characters_view::Message,
    ) -> Task<Message> {
        let project_root = self.sidebar.project_root().map(std::path::Path::to_path_buf);
        let reaction = self.characters_view.update(
            msg,
            self.project.as_ref(),
            project_root.as_deref(),
        );
        let task = reaction.task.map(Message::CharactersView);
        if reaction.fs_changed {
            // Re-index the project and refresh the card list so any new
            // aliases / mentions land before the next view.
            self.run_import();
            if let Some(p) = self.project.as_ref() {
                self.characters_view.refresh_cards(p);
            }
            self.refresh_entities_in_scene();
        }
        task
    }

    fn handle_timeline_view_message(
        &mut self,
        msg: timeline_view::Message,
    ) -> Task<Message> {
        let project_root = self.sidebar.project_root().map(std::path::Path::to_path_buf);
        let reaction = self.timeline_view.update(msg, project_root.as_deref());
        let mut tasks: Vec<Task<Message>> = Vec::new();
        if let Some(path) = reaction.open_document {
            self.main_view = MainView::Editor;
            if let Some(root) = self.sidebar.project_root() {
                let root = root.to_path_buf();
                tasks.push(Editor::open_path(root, path).map(Message::Editor));
            }
        }
        Task::batch(tasks)
    }

    fn handle_research_view_message(
        &mut self,
        msg: research_view::Message,
    ) -> Task<Message> {
        let project_root = self.sidebar.project_root().map(std::path::Path::to_path_buf);
        let reaction = self.research_view.update(
            msg,
            self.project.as_ref(),
            project_root.as_deref(),
        );
        let task = reaction.task.map(Message::ResearchView);
        if reaction.fs_changed {
            self.run_import();
            if let Some(p) = self.project.as_ref() {
                self.research_view.refresh_cards(p);
            }
            self.refresh_entities_in_scene();
        }
        task
    }

    fn handle_relationships_view_message(
        &mut self,
        msg: relationships_view::Message,
    ) -> Task<Message> {
        let project_root = self.sidebar.project_root().map(std::path::Path::to_path_buf);
        let reaction = self
            .relationships_view
            .update(msg, project_root.as_deref());
        let mut tasks: Vec<Task<Message>> = Vec::new();
        if let Some(path) = reaction.open_document {
            self.main_view = MainView::Editor;
            if let Some(root) = self.sidebar.project_root() {
                let root = root.to_path_buf();
                tasks.push(Editor::open_path(root, path).map(Message::Editor));
            }
        }
        Task::batch(tasks)
    }

    fn handle_corkboard_view_message(
        &mut self,
        msg: corkboard_view::Message,
    ) -> Task<Message> {
        let project_root = self.sidebar.project_root().map(std::path::Path::to_path_buf);
        let reaction = self.corkboard_view.update(
            msg,
            self.project.as_mut(),
            project_root.as_deref(),
        );
        let mut tasks: Vec<Task<Message>> = Vec::new();
        if reaction.fs_changed {
            self.run_import();
            if let (Some(p), Some(root)) = (self.project.as_ref(), project_root.as_deref()) {
                self.corkboard_view.refresh(p, root);
            }
        }
        if let Some(path) = reaction.open_document {
            self.main_view = MainView::Editor;
            if let Some(root) = self.sidebar.project_root() {
                let root = root.to_path_buf();
                tasks.push(Editor::open_path(root, path).map(Message::Editor));
            }
        }
        Task::batch(tasks)
    }

    fn handle_locations_view_message(
        &mut self,
        msg: locations_view::Message,
    ) -> Task<Message> {
        let project_root = self.sidebar.project_root().map(std::path::Path::to_path_buf);
        let reaction = self.locations_view.update(
            msg,
            self.project.as_ref(),
            project_root.as_deref(),
        );
        let task = reaction.task.map(Message::LocationsView);
        if reaction.fs_changed {
            self.run_import();
            if let Some(p) = self.project.as_ref() {
                self.locations_view.refresh_cards(p);
            }
            self.refresh_entities_in_scene();
        }
        task
    }

    fn handle_assistant_message(&mut self, msg: assistant::Message) -> Task<Message> {
        let is_api_submit = matches!(msg, assistant::Message::ApiKeySubmit);
        // Re-detect mentions when the user opens the Suggestions tab so
        // they always see results for the current buffer, not the state
        // as of the last autosave. Cheap: scan + replace name_match rows.
        let is_suggestions_tab = matches!(
            &msg,
            assistant::Message::TabSelected(assistant::Tab::Suggestions),
        );
        let confirm_id = match &msg {
            assistant::Message::SuggestionConfirm(id) => Some(*id),
            _ => None,
        };
        let reject_id = match &msg {
            assistant::Message::SuggestionReject(id) => Some(*id),
            _ => None,
        };
        let jump_id = match &msg {
            assistant::Message::SuggestionJump(id) => Some(*id),
            _ => None,
        };
        let context = self.build_assistant_context();
        let task = self.assistant.update(msg, context).map(Message::Assistant);
        let mut tasks: Vec<Task<Message>> = vec![task];
        if is_suggestions_tab {
            self.run_mention_detection();
        }
        if let Some(id) = jump_id {
            if let Some(jump_task) = self.jump_to_suggestion(id) {
                tasks.push(jump_task);
            }
        }
        if let Some(id) = confirm_id {
            if let Some(project) = self.project.as_mut() {
                match letswrite_import::confirm(project, id) {
                    Ok(Some(action)) => {
                        // Splice the wiki-link into the editor buffer
                        // only if the affected document is the one
                        // currently open. Otherwise we'd need to load,
                        // edit, save — for now we defer that case.
                        let current_rel = self.editor.snapshot().rel_path;
                        if current_rel.as_deref() == Some(action.rel_path.as_str()) {
                            let splice_task = self
                                .editor
                                .splice_at(
                                    action.start_offset,
                                    action.end_offset,
                                    &action.expected,
                                    action.replacement,
                                )
                                .map(Message::Editor);
                            tasks.push(splice_task);
                        } else {
                            tracing::warn!(
                                rel = %action.rel_path,
                                "confirm: target document not open, prose not rewritten"
                            );
                        }
                    }
                    Ok(None) => {
                        tracing::debug!(id, "confirm: mention already gone");
                    }
                    Err(err) => tracing::warn!(%err, "confirm mention failed"),
                }
            }
            self.refresh_suggestions();
        }
        if let Some(id) = reject_id {
            if let Some(project) = self.project.as_mut() {
                if let Err(err) = letswrite_import::reject(project, id) {
                    tracing::warn!(%err, "reject mention failed");
                }
            }
            self.refresh_suggestions();
        }
        if is_api_submit {
            match self.assistant.peek_api_key_submission() {
                None => {
                    tracing::warn!(
                        "API key save was clicked but the input is empty — \
                         did you paste / type the key first?"
                    );
                }
                Some(key) => {
                    let key_len = key.len();
                    let prefix: String = key.chars().take(7).collect();
                    if let Err(err) = self.credentials.set(ANTHROPIC_API_KEY, &key) {
                        tracing::error!(%err, "could not persist API key");
                    } else {
                        tracing::info!(
                            len = key_len,
                            prefix = %prefix,
                            "API key saved to keyring"
                        );
                        self.assistant.clear_api_key_draft();
                        let agent = build_agent(Arc::clone(&self.credentials));
                        self.assistant
                            .set_agent(agent, !key_present(&*self.credentials));
                    }
                }
            }
        }
        Task::batch(tasks)
    }

    /// Open the suggestion's document (switching to the editor view if
    /// needed) and queue cursor-jump actions to land on the matched span.
    fn jump_to_suggestion(&mut self, mention_id: i64) -> Option<Task<Message>> {
        let suggestion = self
            .load_suggestions()
            .into_iter()
            .find(|s| s.mention_id == mention_id)?;
        let root = self.sidebar.project_root()?.to_path_buf();
        let abs_path = root.join(&suggestion.rel_path);

        self.main_view = MainView::Editor;

        let current_rel = self.editor.snapshot().rel_path;
        let already_open = current_rel.as_deref() == Some(suggestion.rel_path.as_str());

        let jump_task = self
            .editor
            .jump_to_range(suggestion.start_offset, suggestion.end_offset)
            .map(Message::Editor);

        if already_open {
            // Document is in the editor; just jump.
            Some(jump_task)
        } else {
            // Open the document; the editor's load is async so chain the
            // jump after it resolves.
            let open_task = Editor::open_path(root, abs_path).map(Message::Editor);
            Some(Task::batch([open_task, jump_task]))
        }
    }

    fn refresh_entities_in_scene(&mut self) {
        let context = self.build_assistant_context();
        let present_names: Vec<String> = context
            .entities_in_scene
            .iter()
            .filter(|e| e.kind == "character")
            .map(|e| e.name.clone())
            .collect();
        self.assistant.set_entities_in_scene(context.entities_in_scene);
        let all = self.all_character_names();
        self.assistant.set_minimap_state(&all, &present_names);
    }

    fn all_character_names(&self) -> Vec<String> {
        let Some(project) = self.project.as_ref() else {
            return Vec::new();
        };
        let conn = project.database().conn();
        let mut stmt = match conn.prepare(
            "SELECT name FROM entities
              WHERE project_id = ?1 AND kind = 'character'
              ORDER BY name",
        ) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(%err, "character list query failed");
                return Vec::new();
            }
        };
        stmt.query_map(rusqlite::params![project.id()], |r| {
            r.get::<_, String>(0)
        })
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
    }

    fn refresh_suggestions(&mut self) {
        let suggestions = self.load_suggestions();
        self.assistant.set_suggestions(suggestions);
    }

    fn load_suggestions(&self) -> Vec<assistant::PendingSuggestion> {
        let Some(project) = self.project.as_ref() else {
            return Vec::new();
        };
        let Some(rel) = self.editor.snapshot().rel_path else {
            return Vec::new();
        };
        let conn = project.database().conn();
        let mut stmt = match conn.prepare(
            "SELECT em.id, e.name, e.kind, em.start_offset, em.end_offset
               FROM entity_mentions em
               JOIN entities e ON e.id = em.entity_id
               JOIN documents d ON d.id = em.document_id
              WHERE d.project_id = ?1
                AND d.rel_path = ?2
                AND em.source = 'name_match'
              ORDER BY em.start_offset",
        ) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(%err, "suggestion query prepare failed");
                return Vec::new();
            }
        };
        let rows = match stmt.query_map(
            rusqlite::params![project.id(), rel],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        ) {
            Ok(r) => r,
            Err(err) => {
                tracing::warn!(%err, "suggestion query failed");
                return Vec::new();
            }
        };
        let Some(root) = self.sidebar.project_root() else {
            return Vec::new();
        };
        let abs = root.join(&rel);
        let body = std::fs::read_to_string(&abs).unwrap_or_default();
        rows.flatten()
            .map(|(mention_id, entity_name, entity_kind, start, end)| {
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let start_usize = start.max(0) as usize;
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let end_usize = end.max(0) as usize;
                let context_snippet =
                    extract_snippet(&body, start_usize, end_usize);
                assistant::PendingSuggestion {
                    mention_id,
                    entity_name,
                    entity_kind,
                    context_snippet,
                    start_offset: start_usize,
                    end_offset: end_usize,
                    rel_path: rel.clone(),
                }
            })
            .collect()
    }

    fn build_assistant_context(&self) -> AssistantContext {
        let inputs = BuildInputs {
            project: self.project.as_ref(),
            project_root: self.sidebar.project_root(),
            editor: self.editor.snapshot(),
            // Generous default; the agent will trim if it has to.
            token_budget: 16_000,
        };
        context_builder::build(&inputs)
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
    use keyboard::key::{Key, Named};
    match event {
        Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
            Some(Message::ModifiersChanged(m))
        }
        Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
            // Only intercept the global search shortcuts here; everything
            // else (typing into a focused input, navigation in lists)
            // continues to flow to whichever widget has focus.
            match key {
                Key::Character(c) if modifiers.control() && c.eq_ignore_ascii_case("f") => {
                    Some(Message::OpenSearchPanel(search::Mode::Find))
                }
                Key::Character(c) if modifiers.control() && c.eq_ignore_ascii_case("h") => {
                    Some(Message::OpenSearchPanel(search::Mode::Replace))
                }
                Key::Named(Named::Escape) => Some(Message::CloseSearchPanel),
                _ => None,
            }
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

/// Extract a short prose snippet around the matched span for the
/// suggestion-confirmation UI. Returns ~80 chars before + the match +
/// ~80 chars after, clipped to char boundaries to avoid panics on UTF-8.
fn extract_snippet(body: &str, start: usize, end: usize) -> String {
    let snippet_start = clamp_to_char_boundary(body, start.saturating_sub(80));
    let snippet_end = clamp_to_char_boundary(body, (end + 80).min(body.len()));
    let raw = &body[snippet_start..snippet_end];
    raw.replace('\n', " ").trim().to_owned()
}

fn clamp_to_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Build the default Anthropic agent, or `None` if construction fails
/// (which only happens if the HTTP client can't be built — extremely rare).
fn build_agent(credentials: Arc<dyn CredentialStore>) -> Option<Arc<dyn Agent>> {
    match AnthropicProvider::new(credentials) {
        Ok(provider) => {
            // Pick the first available model as the default; the settings
            // UI will let users choose later.
            let model = provider
                .models()
                .first()
                .map_or_else(|| "claude-sonnet-4-6".to_owned(), |m| m.id.clone());
            let provider_arc: Arc<dyn Provider> = Arc::new(provider);
            Some(Arc::new(DefaultAgent::new(provider_arc, model)))
        }
        Err(err) => {
            tracing::error!(%err, "could not build Anthropic provider");
            None
        }
    }
}

fn key_present(credentials: &dyn CredentialStore) -> bool {
    matches!(credentials.get(ANTHROPIC_API_KEY), Ok(Some(_)))
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
