//! Right-column assistant panel.
//!
//! Talks to a [`letswrite_ai::Agent`] via streaming. Renders the
//! conversation as a chat transcript, with the in-progress assistant
//! reply growing in place as tokens arrive. Per the project memory
//! [[project-ai-abstraction]]: the UI imports `Agent` and the event
//! enum, never any provider impl. Provider selection happens at app
//! construction time.

use std::sync::Arc;

use iced::futures::{SinkExt, Stream, StreamExt};
use iced::widget::{button, column, container, markdown, row, scrollable, text, text_input};
use iced::{Element, Length, Task, Theme};

use letswrite_ai::{
    Agent, AgentEvent, AgentInput, AssistantContext, EntityInScene, ProviderError,
};
use tokio_util::sync::CancellationToken;

use crate::minimap::Minimap;
use crate::presets::BUILT_INS;

/// One turn in the conversation: a user input + the assistant's reply
/// (which grows as deltas arrive).
#[derive(Debug, Clone)]
pub(crate) struct Turn {
    pub user: String,
    pub reply: String,
    pub state: TurnState,
    /// Parsed Markdown for the reply; rebuilt as the reply grows.
    pub reply_items: Vec<markdown::Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TurnState {
    /// Request sent, no tokens yet.
    Thinking,
    /// Streaming response in progress.
    Streaming,
    /// Done.
    Done,
    /// Failed mid-turn. The error message is rendered below the reply.
    Failed(String),
    /// User cancelled.
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Chat,
    Characters,
    Suggestions,
}

/// One pending name-match suggestion the user can confirm or reject.
#[derive(Debug, Clone)]
pub(crate) struct PendingSuggestion {
    pub mention_id: i64,
    pub entity_name: String,
    pub matched_text: String,
    /// Short prose context around the match for the confirmation UI.
    pub context_snippet: String,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    InputChanged(String),
    Submit,
    Cancel,
    /// User switched between chat / characters / suggestions tabs.
    TabSelected(Tab),
    /// User accepted a pending name-match suggestion.
    SuggestionConfirm(i64),
    /// User rejected a pending name-match suggestion.
    SuggestionReject(i64),
    /// User picked a preset; fill the composer + tag the next submit.
    PresetSelected(&'static str),
    /// Streamed event from the agent. The `turn` index identifies which
    /// turn the event belongs to (turns are appended in order).
    Stream { turn: usize, event: AgentEventCow },
    /// User entered an API key in the inline credentials prompt.
    ApiKeyChanged(String),
    /// User submitted the API key.
    ApiKeySubmit,
    /// User clicked a link in the rendered markdown reply.
    LinkClicked(markdown::Url),
}

/// `AgentEvent` isn't `Clone`-friendly across `iced::Subscription` boundaries
/// without paying for the `Arc`s on every event; wrap it so the message
/// stays cheap to send.
#[derive(Debug, Clone)]
pub(crate) struct AgentEventCow(pub Arc<AgentEvent>);

#[derive(Debug)]
pub(crate) struct Assistant {
    agent: Option<Arc<dyn Agent>>,
    /// The conversation. v1 is in-memory only — persistence to `ai_threads`
    /// lands as a follow-up once we know what the UX needs.
    turns: Vec<Turn>,
    /// Currently-typed input.
    draft: String,
    /// Preset id staged for the next submit, if the user clicked one.
    pending_preset: Option<&'static str>,
    /// Token to cancel the in-flight turn, if any.
    in_flight: Option<CancellationToken>,
    /// Inline API-key prompt when no key is configured.
    api_key_draft: String,
    /// `true` if the configured provider needs a credential we don't have.
    needs_api_key: bool,
    /// Which tab is showing.
    tab: Tab,
    /// Characters present in the current scene; refreshed on each
    /// assistant interaction (the context-builder query already runs).
    entities_in_scene: Vec<EntityInScene>,
    /// Pending `name_match` suggestions for the open document.
    suggestions: Vec<PendingSuggestion>,
    /// Character constellation rendered as a header strip.
    minimap: Minimap,
}

impl Assistant {
    pub(crate) fn new(agent: Option<Arc<dyn Agent>>, needs_api_key: bool) -> Self {
        Self {
            agent,
            turns: Vec::new(),
            draft: String::new(),
            pending_preset: None,
            in_flight: None,
            api_key_draft: String::new(),
            needs_api_key,
            tab: Tab::Chat,
            entities_in_scene: Vec::new(),
            suggestions: Vec::new(),
            minimap: Minimap::new(),
        }
    }

    /// Update the minimap's constellation: every character in the project,
    /// plus which of those are present in the current scene.
    pub(crate) fn set_minimap_state(
        &mut self,
        all_characters: &[String],
        present_names: &[String],
    ) {
        self.minimap.set_state(all_characters, present_names);
    }

    /// Refresh the characters-in-scene list (called by the shell when the
    /// editor's snapshot changes or context is rebuilt).
    pub(crate) fn set_entities_in_scene(&mut self, entities: Vec<EntityInScene>) {
        self.entities_in_scene = entities;
    }

    pub(crate) fn set_suggestions(&mut self, suggestions: Vec<PendingSuggestion>) {
        self.suggestions = suggestions;
    }

    pub(crate) fn set_agent(&mut self, agent: Option<Arc<dyn Agent>>, needs_api_key: bool) {
        self.agent = agent;
        self.needs_api_key = needs_api_key;
    }

    /// Hook to receive the API-key value the user typed. The app shell
    /// is responsible for actually persisting it to the keyring.
    pub(crate) fn take_api_key_submission(&mut self) -> Option<String> {
        if self.api_key_draft.trim().is_empty() {
            return None;
        }
        let key = std::mem::take(&mut self.api_key_draft);
        Some(key.trim().to_owned())
    }

    pub(crate) fn update(
        &mut self,
        message: Message,
        context_for_next_turn: AssistantContext,
    ) -> Task<Message> {
        match message {
            Message::InputChanged(s) => {
                // If the user edits the composer manually, drop any preset
                // tag — the input is no longer the preset's exact prompt.
                // (A future task could keep the preset tag if the edit
                // looks like a small addition; this is simpler and correct.)
                if Some(s.as_str()) != BUILT_INS.iter().find(|p| Some(p.id) == self.pending_preset).map(|p| p.prompt) {
                    self.pending_preset = None;
                }
                self.draft = s;
                Task::none()
            }
            Message::Submit => self.start_turn(context_for_next_turn),
            Message::PresetSelected(id) => {
                if let Some(preset) = BUILT_INS.iter().find(|p| p.id == id) {
                    // Populate the composer; user can still edit before sending.
                    self.draft = preset.prompt.to_owned();
                    self.pending_preset = Some(preset.id);
                }
                Task::none()
            }
            Message::Cancel => {
                if let Some(tok) = self.in_flight.take() {
                    tok.cancel();
                }
                Task::none()
            }
            Message::TabSelected(tab) => {
                self.tab = tab;
                Task::none()
            }
            Message::SuggestionConfirm(_) | Message::SuggestionReject(_) => {
                // The shell handles the DB write and re-supplies the
                // suggestion list. We just consume the message.
                Task::none()
            }
            Message::ApiKeyChanged(s) => {
                self.api_key_draft = s;
                Task::none()
            }
            Message::ApiKeySubmit => {
                // The app shell observes via take_api_key_submission().
                Task::none()
            }
            Message::Stream { turn, event } => {
                self.apply_stream_event(turn, &event.0);
                Task::none()
            }
            Message::LinkClicked(url) => {
                tracing::info!(url = %url, "assistant reply link clicked");
                Task::none()
            }
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let mut col = column![].spacing(8).padding(12).width(Length::Fill);

        if self.needs_api_key {
            col = col.push(api_key_prompt(&self.api_key_draft));
            col = col.push(text("(Set your Anthropic API key to enable the assistant.)").size(11));
        }

        col = col.push(self.minimap.view());
        col = col.push(tab_bar(
            self.tab,
            self.entities_in_scene.len(),
            self.suggestions.len(),
        ));

        match self.tab {
            Tab::Chat => {
                let mut transcript = column![].spacing(12).width(Length::Fill);
                for turn in &self.turns {
                    transcript = transcript.push(render_turn(turn));
                }
                col = col.push(
                    scrollable(transcript)
                        .height(Length::Fill)
                        .width(Length::Fill),
                );
                col = col.push(preset_toolbar(self.pending_preset));
                col = col.push(composer_row(
                    &self.draft,
                    self.in_flight.is_some(),
                    self.agent.is_some(),
                ));
            }
            Tab::Characters => {
                col = col.push(
                    scrollable(characters_view(&self.entities_in_scene))
                        .height(Length::Fill)
                        .width(Length::Fill),
                );
            }
            Tab::Suggestions => {
                col = col.push(
                    scrollable(suggestions_view(&self.suggestions))
                        .height(Length::Fill)
                        .width(Length::Fill),
                );
            }
        }

        container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    // -- internals -----------------------------------------------------------

    fn start_turn(&mut self, context: AssistantContext) -> Task<Message> {
        let Some(agent) = self.agent.clone() else {
            tracing::warn!("no agent configured; can't submit");
            return Task::none();
        };
        let user_text = std::mem::take(&mut self.draft);
        if user_text.trim().is_empty() {
            return Task::none();
        }
        let preset = self.pending_preset.take().map(str::to_owned);
        let cancel = CancellationToken::new();
        self.in_flight = Some(cancel.clone());
        let turn_index = self.turns.len();
        self.turns.push(Turn {
            user: user_text.clone(),
            reply: String::new(),
            state: TurnState::Thinking,
            reply_items: Vec::new(),
        });

        let input = AgentInput {
            user_input: user_text,
            preset,
        };

        // Spawn a stream subscription: every AgentEvent becomes a
        // Message::Stream tagged with this turn's index.
        Task::run(
            stream_from_agent(agent, input, context, cancel),
            move |event| Message::Stream {
                turn: turn_index,
                event: AgentEventCow(Arc::new(event)),
            },
        )
    }

    fn apply_stream_event(&mut self, turn_idx: usize, event: &AgentEvent) {
        let Some(turn) = self.turns.get_mut(turn_idx) else {
            return;
        };
        match event {
            AgentEvent::Thinking => {
                turn.state = TurnState::Thinking;
            }
            AgentEvent::TextChunk(chunk) => {
                if turn.state == TurnState::Thinking {
                    turn.state = TurnState::Streaming;
                }
                turn.reply.push_str(chunk);
                turn.reply_items = markdown::parse(&turn.reply).collect();
            }
            AgentEvent::ToolInvocation { name, .. } => {
                use std::fmt::Write as _;
                let _ = write!(turn.reply, "\n_(calling tool: `{name}`)_\n");
                turn.reply_items = markdown::parse(&turn.reply).collect();
            }
            AgentEvent::ToolResult { name, .. } => {
                use std::fmt::Write as _;
                let _ = write!(turn.reply, "\n_(tool result: `{name}`)_\n");
                turn.reply_items = markdown::parse(&turn.reply).collect();
            }
            AgentEvent::Suggestion { kind, .. } => {
                tracing::info!(kind, "agent suggestion received (no UI yet)");
            }
            AgentEvent::Done { .. } => {
                turn.state = TurnState::Done;
                self.in_flight = None;
            }
            AgentEvent::Error(err) => {
                turn.state = if matches!(err, ProviderError::Cancelled) {
                    TurnState::Cancelled
                } else {
                    TurnState::Failed(err.to_string())
                };
                self.in_flight = None;
            }
        }
    }
}

fn api_key_prompt(draft: &str) -> Element<'_, Message> {
    column![
        text("Anthropic API key").size(13),
        text_input("sk-…", draft)
            .on_input(Message::ApiKeyChanged)
            .on_submit(Message::ApiKeySubmit)
            .size(12),
        button(text("Save key").size(12))
            .on_press(Message::ApiKeySubmit)
            .style(button::primary),
    ]
    .spacing(4)
    .into()
}

fn tab_bar(
    active: Tab,
    character_count: usize,
    suggestion_count: usize,
) -> Element<'static, Message> {
    let chat_style = if active == Tab::Chat { button::primary } else { button::secondary };
    let chars_style = if active == Tab::Characters { button::primary } else { button::secondary };
    let suggest_style = if active == Tab::Suggestions { button::primary } else { button::secondary };
    let chars_label = if character_count > 0 {
        format!("In scene ({character_count})")
    } else {
        "In scene".to_owned()
    };
    let suggest_label = if suggestion_count > 0 {
        format!("Suggestions ({suggestion_count})")
    } else {
        "Suggestions".to_owned()
    };
    container(
        iced::widget::Row::new()
            .spacing(4)
            .push(
                button(text("Chat").size(12))
                    .style(chat_style)
                    .on_press(Message::TabSelected(Tab::Chat)),
            )
            .push(
                button(text(chars_label).size(12))
                    .style(chars_style)
                    .on_press(Message::TabSelected(Tab::Characters)),
            )
            .push(
                button(text(suggest_label).size(12))
                    .style(suggest_style)
                    .on_press(Message::TabSelected(Tab::Suggestions)),
            ),
    )
    .into()
}

fn suggestions_view(suggestions: &[PendingSuggestion]) -> Element<'_, Message> {
    if suggestions.is_empty() {
        return column![
            text("No pending suggestions.").size(12),
            text(
                "When you save a document, letswrite scans it for character / location names \
                 that aren't yet wiki-linked. Matches show up here so you can confirm or \
                 reject them.",
            )
            .size(11),
        ]
        .spacing(6)
        .into();
    }
    let mut col = column![].spacing(8);
    for suggestion in suggestions {
        col = col.push(render_suggestion_card(suggestion));
    }
    col.into()
}

fn render_suggestion_card(s: &PendingSuggestion) -> Element<'_, Message> {
    let id = s.mention_id;
    container(
        column![
            text(format!("{} → {}", s.matched_text, s.entity_name)).size(13),
            text(s.context_snippet.clone()).size(11),
            iced::widget::Row::new()
                .spacing(4)
                .push(
                    button(text("Confirm").size(11))
                        .style(button::primary)
                        .on_press(Message::SuggestionConfirm(id)),
                )
                .push(
                    button(text("Reject").size(11))
                        .style(button::secondary)
                        .on_press(Message::SuggestionReject(id)),
                ),
        ]
        .spacing(4),
    )
    .padding(8)
    .style(assistant_bubble_style)
    .width(Length::Fill)
    .into()
}

fn characters_view(entities: &[EntityInScene]) -> Element<'_, Message> {
    if entities.is_empty() {
        return column![
            text("No characters detected in the current scene.").size(12),
            text(
                "Wiki-link a character (e.g. `[[Evan Calder]]`) or place the cursor in a scene \
                 that already mentions one.",
            )
            .size(11),
        ]
        .spacing(6)
        .into();
    }
    let mut col = column![].spacing(8);
    for entity in entities {
        col = col.push(render_entity_card(entity));
    }
    col.into()
}

fn render_entity_card(entity: &EntityInScene) -> Element<'_, Message> {
    container(
        column![
            text(entity.name.clone()).size(13),
            text(entity.kind.clone()).size(10),
            text(entity.current_state.clone()).size(11),
        ]
        .spacing(2),
    )
    .padding(8)
    .style(assistant_bubble_style)
    .width(Length::Fill)
    .into()
}

fn preset_toolbar(active: Option<&'static str>) -> Element<'static, Message> {
    let mut row = iced::widget::Row::new().spacing(4).padding([4, 0]);
    for preset in BUILT_INS {
        let style = if Some(preset.id) == active {
            button::primary
        } else {
            button::secondary
        };
        row = row.push(
            button(text(preset.label).size(11))
                .style(style)
                .on_press(Message::PresetSelected(preset.id)),
        );
    }
    container(row.wrap()).into()
}

fn composer_row<'a>(
    draft: &'a str,
    busy: bool,
    has_agent: bool,
) -> Element<'a, Message> {
    let mut input = text_input("Ask the assistant…", draft)
        .on_input(Message::InputChanged)
        .size(13)
        .width(Length::Fill);
    if !busy && has_agent {
        input = input.on_submit(Message::Submit);
    }

    let action: Element<'a, Message> = if busy {
        button(text("Cancel").size(12))
            .on_press(Message::Cancel)
            .style(button::danger)
            .into()
    } else {
        let mut btn = button(text("Send").size(12)).style(button::primary);
        if has_agent {
            btn = btn.on_press(Message::Submit);
        }
        btn.into()
    };

    row![input, action].spacing(4).into()
}

fn render_turn(turn: &Turn) -> Element<'_, Message> {
    let user_block = container(text(turn.user.clone()).size(13))
        .padding(8)
        .style(user_bubble_style);

    let mut reply: Vec<Element<'_, Message>> = Vec::new();
    if turn.reply_items.is_empty() {
        let placeholder = match turn.state {
            TurnState::Thinking => "Thinking…",
            TurnState::Done => "(no reply)",
            TurnState::Streaming | TurnState::Failed(_) | TurnState::Cancelled => "",
        };
        if !placeholder.is_empty() {
            reply.push(text(placeholder).size(12).into());
        }
    } else {
        reply.push(
            markdown::view(
                &turn.reply_items,
                markdown::Settings::with_text_size(13),
                markdown::Style::from_palette(Theme::Dark.palette()),
            )
            .map(Message::LinkClicked),
        );
    }
    match &turn.state {
        TurnState::Failed(msg) => {
            reply.push(text(format!("⚠ {msg}")).size(12).into());
        }
        TurnState::Cancelled => {
            reply.push(text("(cancelled)").size(12).into());
        }
        _ => {}
    }

    let reply_block = container(column(reply).spacing(4))
        .padding(8)
        .style(assistant_bubble_style);

    column![user_block, reply_block]
        .spacing(4)
        .width(Length::Fill)
        .into()
}

fn user_bubble_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(iced::Background::Color(palette.primary.weak.color)),
        text_color: Some(palette.primary.weak.text),
        border: iced::Border { radius: 4.0.into(), ..Default::default() },
        ..container::Style::default()
    }
}

fn assistant_bubble_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(iced::Background::Color(palette.background.weak.color)),
        text_color: Some(palette.background.weak.text),
        border: iced::Border { radius: 4.0.into(), ..Default::default() },
        ..container::Style::default()
    }
}

/// Adapt the agent's stream into something Iced's `Task::run` consumes.
fn stream_from_agent(
    agent: Arc<dyn Agent>,
    input: AgentInput,
    context: AssistantContext,
    cancel: CancellationToken,
) -> impl Stream<Item = AgentEvent> {
    iced::stream::channel(64, move |mut tx| async move {
        let mut s = agent.ask(input, context, cancel).await;
        while let Some(ev) = s.next().await {
            if tx.send(ev).await.is_err() {
                break;
            }
        }
    })
}
