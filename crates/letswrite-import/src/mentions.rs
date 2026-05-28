//! Derive `entity_mentions` rows from `[[wiki-link]]` references in any
//! document's body.
//!
//! Resolution is purely by name: a wiki-link `[[Target]]` matches an entity
//! whose `name` equals `Target` (case-insensitive), or whose `aliases_json`
//! contains `Target`. The `[[Target|Display]]` piped form is supported —
//! only the target side is resolved; the display side is the user's
//! prose-side rendering choice and doesn't affect resolution.
//!
//! Unresolved links are logged at WARN with the target string. They don't
//! abort the import; the user can fix typos and re-import.

use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::params;

use letswrite_core::{Document, DocumentKind, Result};

#[allow(clippy::redundant_pub_crate)]
pub(crate) struct Outcome {
    pub mentions: usize,
    pub unresolved: usize,
}

#[allow(clippy::redundant_pub_crate)]
pub(crate) fn import(
    tx: &rusqlite::Transaction<'_>,
    project_id: i64,
    docs: &[(DocumentKind, PathBuf, Document)],
) -> Result<Outcome> {
    let index = build_entity_index(tx, project_id)?;
    let mut inserted = 0;
    let mut unresolved = 0;

    for (_, _, doc) in docs {
        let document_id = super::document_id(tx, project_id, &doc.rel_path)?;
        for link in find_wiki_links(&doc.body) {
            let lookup = link.target.trim().to_ascii_lowercase();
            let Some(&entity_id) = index.get(&lookup) else {
                tracing::warn!(
                    document = %doc.rel_path,
                    target = %link.target,
                    "wiki-link target not found in entities"
                );
                unresolved += 1;
                continue;
            };
            // See scenes.rs — usize → i64 is safe for any realistic file size.
            #[allow(clippy::cast_possible_wrap)]
            let start = link.start_offset as i64;
            #[allow(clippy::cast_possible_wrap)]
            let end = link.end_offset as i64;
            tx.execute(
                "INSERT INTO entity_mentions
                    (document_id, entity_id, start_offset, end_offset, source, confidence)
                 VALUES (?1, ?2, ?3, ?4, 'explicit_tag', 1.0)",
                params![document_id, entity_id, start, end],
            )?;
            inserted += 1;
        }
    }

    Ok(Outcome { mentions: inserted, unresolved })
}

/// Lower-cased name + aliases → `entity_id`, for fast resolution.
fn build_entity_index(
    tx: &rusqlite::Transaction<'_>,
    project_id: i64,
) -> Result<HashMap<String, i64>> {
    let mut stmt = tx.prepare(
        "SELECT id, name, aliases_json FROM entities WHERE project_id = ?1",
    )?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut index: HashMap<String, i64> = HashMap::new();
    for (id, name, aliases_json) in rows {
        index.insert(name.trim().to_ascii_lowercase(), id);
        if let Ok(aliases) = serde_json::from_str::<Vec<String>>(&aliases_json) {
            for alias in aliases {
                index.insert(alias.trim().to_ascii_lowercase(), id);
            }
        }
    }
    Ok(index)
}

#[derive(Debug, PartialEq, Eq)]
struct WikiLink {
    target: String,
    /// Byte offset of the `[[` in `body`.
    start_offset: usize,
    /// Byte offset just past the `]]`.
    end_offset: usize,
}

/// Find every `[[Target]]`, `[[Target|Display]]`, or `[[Target: Display]]`
/// in `body`. Mid-edit unclosed `[[`s are skipped (no closing `]]`),
/// matching the editor's scanner. The `": "` (colon + space) variant is
/// emitted by the Confirm-from-suggestion flow; a bare colon without a
/// trailing space is part of the target (so `"Chapter 1: Intro"` works as
/// a title-style link target).
fn find_wiki_links(body: &str) -> Vec<WikiLink> {
    let bytes = body.as_bytes();
    let n = bytes.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < n {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(close) = find_double_close(body, i + 2) {
                let inner = &body[i + 2..close];
                let target = if let Some((t, _)) = inner.split_once('|') {
                    t.trim()
                } else if let Some((t, _)) = inner.split_once(": ") {
                    t.trim()
                } else {
                    inner
                };
                out.push(WikiLink {
                    target: target.to_owned(),
                    start_offset: i,
                    end_offset: close + 2,
                });
                i = close + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn find_double_close(s: &str, from: usize) -> Option<usize> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_plain_wiki_links() {
        let body = "Talked with [[Evan Calder]] and then [[Aletheia]] later.";
        let hits = find_wiki_links(body);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].target, "Evan Calder");
        assert_eq!(hits[1].target, "Aletheia");
    }

    #[test]
    fn piped_target_extracted() {
        let body = "see [[Evan Calder|Evan]] in the hall.";
        let hits = find_wiki_links(body);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, "Evan Calder");
    }

    #[test]
    fn colon_space_alias_target_extracted() {
        // Confirm-from-suggestion writes `[[Entity: matched]]`. The
        // mention resolver must pick the entity name, not the alias.
        let body = "see [[Evan Calder: Evan]] in the hall.";
        let hits = find_wiki_links(body);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, "Evan Calder");
    }

    #[test]
    fn bare_colon_target_is_kept_whole() {
        // No trailing space → not an alias separator.
        let body = "see [[Chapter:Intro]] today.";
        let hits = find_wiki_links(body);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, "Chapter:Intro");
    }

    #[test]
    fn unclosed_link_is_ignored() {
        let body = "writer typed [[unfinished";
        assert!(find_wiki_links(body).is_empty());
    }

    #[test]
    fn offsets_round_trip_to_source_substring() {
        let body = "x [[Evan]] y";
        let hit = &find_wiki_links(body)[0];
        assert_eq!(&body[hit.start_offset..hit.end_offset], "[[Evan]]");
    }
}
