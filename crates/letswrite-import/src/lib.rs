//! Importers for letswrite projects.
//!
//! Today the only importer is the Markdown files importer ([`import_project`]),
//! which takes an already-opened [`letswrite_core::Project`] and populates
//! its index with entities (from `Characters/`, `Locations/`), scene
//! breakdowns (from `## Beat N:` headings inside chapter files), and
//! `[[wiki-link]]` mentions.
//!
//! The importer is destructive on the index tables it owns
//! (`entities`, `scenes`, `entity_mentions`) — it wipes them for the
//! project and rebuilds from scratch. The on-disk Markdown is the source
//! of truth; if the index is stale, you re-import.

mod entities;
mod mentions;
mod scenes;

use rusqlite::params;

use letswrite_core::{Document, DocumentKind, Error, Project, Result};

/// Outcome of an import run. Counts are derived from the actual rows that
/// landed in the index, so they're useful both for UI feedback and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub characters: usize,
    pub locations: usize,
    pub scenes: usize,
    pub mentions: usize,
    /// Wiki-link targets that didn't match any known entity. Just a count;
    /// individual misses are logged at WARN.
    pub unresolved_mentions: usize,
}

/// Run a full import over `project`. Wipes and rebuilds `entities`,
/// `scenes`, and `entity_mentions` for this project from the current
/// on-disk Markdown.
pub fn import_project(project: &mut Project) -> Result<ImportReport> {
    let files = project.scan();
    let project_id = project.id();
    let root = project.root().to_path_buf();

    // Pre-load every file into a typed Document. We do this *before* the
    // transaction so an unparseable frontmatter aborts cleanly instead of
    // leaving a half-populated index.
    let mut docs: Vec<(DocumentKind, std::path::PathBuf, Document)> =
        Vec::with_capacity(files.len());
    for file in files {
        match Document::load(&root, &file.path) {
            Ok(doc) => docs.push((file.kind, file.path, doc)),
            Err(err) => {
                tracing::warn!(
                    path = %file.path.display(),
                    %err,
                    "skipping unreadable file"
                );
            }
        }
    }

    let conn = project.database_mut().conn_mut();
    let tx = conn.transaction()?;

    // Wipe the index tables for this project; foreign keys carry the cascade
    // through entity_mentions, scenes, timeline_entries, relationships.
    tx.execute(
        "DELETE FROM entities WHERE project_id = ?1",
        params![project_id],
    )?;
    // scenes is keyed by document, so they survive an entity wipe but get
    // dropped here for a clean re-derivation.
    tx.execute(
        "DELETE FROM scenes
          WHERE document_id IN (SELECT id FROM documents WHERE project_id = ?1)",
        params![project_id],
    )?;
    tx.execute(
        "DELETE FROM entity_mentions
          WHERE document_id IN (SELECT id FROM documents WHERE project_id = ?1)",
        params![project_id],
    )?;

    let entities_outcome = entities::import(&tx, project_id, &docs)?;
    let scenes_outcome = scenes::import(&tx, project_id, &docs)?;
    let mentions_outcome = mentions::import(&tx, project_id, &docs)?;

    tx.commit()?;

    let report = ImportReport {
        characters: entities_outcome.characters,
        locations: entities_outcome.locations,
        scenes: scenes_outcome.scenes,
        mentions: mentions_outcome.mentions,
        unresolved_mentions: mentions_outcome.unresolved,
    };
    tracing::info!(?report, "import completed");
    Ok(report)
}

/// Look up a document's id by `rel_path` inside the given project. Returns an
/// error if the row isn't there — callers should have indexed the project
/// before importing.
fn document_id(
    tx: &rusqlite::Transaction<'_>,
    project_id: i64,
    rel_path: &str,
) -> Result<i64> {
    tx.query_row(
        "SELECT id FROM documents WHERE project_id = ?1 AND rel_path = ?2",
        params![project_id, rel_path],
        |row| row.get(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::InvalidData(format!(
            "document {rel_path} is missing from the index; reindex first"
        )),
        other => other.into(),
    })
}
