//! Phase-3 structural views — what shows up in the editor pane when the
//! user isn't editing a single document.
//!
//! The shell picks a [`MainView`] based on a sidebar toggle. The "Editor"
//! view is the existing single-document Markdown editor; the rest are
//! built up across #18–#24.

pub(crate) mod characters;
pub(crate) mod corkboard;
pub(crate) mod locations;
pub(crate) mod timeline;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MainView {
    /// The single-document Markdown editor (default).
    #[default]
    Editor,
    /// All characters in the project — cards + structured editor.
    Characters,
    /// All locations in the project.
    Locations,
    /// Scene cards / corkboard.
    Corkboard,
    /// Plot / timeline.
    Timeline,
}

// MainView::label() will be useful once we render a tab strip with the
// current view's name; for now the sidebar buttons are the only UI.

