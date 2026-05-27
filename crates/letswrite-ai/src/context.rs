//! `AssistantContext` — the structured bundle the UI hands to an [`Agent`].
//!
//! The Agent decides how to render context into messages. The UI never
//! constructs a `ChatRequest` itself — it builds a context object and lets
//! the agent take it from there. That way the same context shape can be
//! reused by different agents (critique, continuity-check, pacing) and by
//! future providers without UI changes.

use std::path::PathBuf;

use unic_langid::LanguageIdentifier;

/// A bundle of context the agent will use to render a request.
#[derive(Debug, Clone, Default)]
pub struct AssistantContext {
    /// What document the user is editing. `None` if no file is open.
    pub document: Option<DocumentContext>,
    /// What text the user has selected (in either column). Used to focus
    /// the assistant on a specific span.
    pub selection: Option<String>,
    /// Entities present in the current scene (characters + their state).
    /// Empty if no scene is identified yet.
    pub entities_in_scene: Vec<EntityInScene>,
    /// Project-wide meta the agent should know: writing guide, author's
    /// statement, etc. Free-form for now; refined as we use it.
    pub project_meta: Vec<ProjectMeta>,
    /// Language the user is writing in. Either declared via document
    /// frontmatter or detected; influences the assistant's reply language.
    pub language: Option<LanguageIdentifier>,
    /// Maximum number of tokens the agent is allowed to spend on the
    /// context portion (not the response). Agents trim oldest first.
    pub token_budget: u32,
}

/// One open-document context.
#[derive(Debug, Clone)]
pub struct DocumentContext {
    pub abs_path: PathBuf,
    pub rel_path: String,
    pub title: String,
    /// Full body text — the agent picks the window it sends.
    pub body: String,
    /// Cursor position as a byte offset into `body`.
    pub cursor_offset: usize,
    /// Sliding window of text around the cursor. Computed by the UI as a
    /// hint; the agent may choose a different window if budget allows.
    pub window: ContextWindow,
}

/// A precomputed slice of the document the UI thinks is most relevant.
#[derive(Debug, Clone, Default)]
pub struct ContextWindow {
    pub kind: WindowKind,
    /// Byte offsets into `DocumentContext::body`. Half-open.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowKind {
    /// Whole document fits in budget.
    #[default]
    WholeDocument,
    /// The current paragraph only.
    Paragraph,
    /// The current scene (between `## Beat` markers).
    Scene,
    /// The current chapter.
    Chapter,
}

/// One character/location/etc in the current scene.
#[derive(Debug, Clone)]
pub struct EntityInScene {
    pub name: String,
    pub kind: String,
    /// What the entity's role/motivation/state is at this point in the
    /// story. Pre-resolved against the timeline so the agent gets
    /// "motivation as of chapter 12", not "latest motivation".
    pub current_state: String,
}

/// One global piece of project context.
#[derive(Debug, Clone)]
pub struct ProjectMeta {
    pub label: String,
    pub content: String,
}

impl AssistantContext {
    /// Empty context — placeholder for tests and "no project open" UI paths.
    pub fn empty() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_context_is_ergonomic() {
        let ctx = AssistantContext::empty();
        assert!(ctx.document.is_none());
        assert!(ctx.entities_in_scene.is_empty());
        assert_eq!(ctx.token_budget, 0);
    }
}
