//! On-demand entity-mention detector.
//!
//! Unlike the full importer (which wipes-and-rebuilds), this is for the
//! editing path: after a document is saved, we scan its body for names
//! and aliases of *known* entities that aren't already wrapped in a
//! `[[wiki-link]]`. New finds become pending suggestions
//! (`source='name_match'`, `confidence=0.5`) that the user can promote
//! to `user_confirmed` or reject.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::params;

use letswrite_core::{Document, Project, Result};

/// One detected match in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub entity_id: i64,
    pub start_offset: usize,
    pub end_offset: usize,
    /// The exact text matched (for showing in the confirmation UI).
    pub matched_text: String,
}

/// Scan a single document on disk for entity name/alias matches that
/// aren't already explicitly wiki-linked. Inserts each as a pending
/// `name_match` mention with confidence 0.5.
///
/// Idempotent: running again won't duplicate suggestions for the same
/// (document, entity, offset) tuple — we delete prior `name_match` rows
/// for this document first.
pub fn detect_for_document(project: &mut Project, abs_path: &Path) -> Result<usize> {
    let root = project.root().to_path_buf();
    let doc = Document::load(&root, abs_path)?;
    let project_id = project.id();

    let conn = project.database_mut().conn_mut();
    let document_id: i64 = conn.query_row(
        "SELECT id FROM documents WHERE project_id = ?1 AND rel_path = ?2",
        params![project_id, doc.rel_path],
        |r| r.get(0),
    )?;

    let entities = load_entities(conn, project_id)?;
    let explicit_spans = collect_explicit_spans(&doc.body);
    let detections = scan(&doc.body, &entities, &explicit_spans);

    let tx = conn.transaction()?;
    // Clear stale name_match rows for this document — keeps the
    // suggestion list fresh on every save.
    tx.execute(
        "DELETE FROM entity_mentions
          WHERE document_id = ?1 AND source = 'name_match'",
        params![document_id],
    )?;

    let mut count = 0;
    for d in &detections {
        // SQLite stores byte offsets; usize → i64 is safe at any realistic
        // document size.
        #[allow(clippy::cast_possible_wrap)]
        let start = d.start_offset as i64;
        #[allow(clippy::cast_possible_wrap)]
        let end = d.end_offset as i64;
        tx.execute(
            "INSERT INTO entity_mentions
                (document_id, entity_id, start_offset, end_offset, source, confidence)
             VALUES (?1, ?2, ?3, ?4, 'name_match', 0.5)",
            params![document_id, d.entity_id, start, end],
        )?;
        count += 1;
    }
    tx.commit()?;
    Ok(count)
}

/// Entity lookup forms sorted longest-first so multi-word names match
/// before their constituent aliases ("Evan Calder" before "Evan").
struct EntityIndex {
    forms_sorted: Vec<(String, i64)>,
}

fn load_entities(
    conn: &rusqlite::Connection,
    project_id: i64,
) -> Result<EntityIndex> {
    let mut stmt = conn.prepare(
        "SELECT id, name, aliases_json FROM entities WHERE project_id = ?1",
    )?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut by_form: HashMap<String, i64> = HashMap::new();
    for (id, name, aliases_json) in rows {
        by_form.insert(name.trim().to_ascii_lowercase(), id);
        if let Ok(aliases) = serde_json::from_str::<Vec<String>>(&aliases_json) {
            for alias in aliases {
                by_form.insert(alias.trim().to_ascii_lowercase(), id);
            }
        }
    }

    let mut forms_sorted: Vec<(String, i64)> = by_form.into_iter().collect();
    forms_sorted.sort_by_key(|(form, _)| std::cmp::Reverse(form.len()));
    Ok(EntityIndex { forms_sorted })
}

/// Find every byte-range in `body` that's inside an `[[…]]` wiki-link.
/// Matches inside these ranges are skipped — the importer already
/// records them as `explicit_tag`.
fn collect_explicit_spans(body: &str) -> Vec<(usize, usize)> {
    let bytes = body.as_bytes();
    let n = bytes.len();
    let mut spans = Vec::new();
    let mut i = 0;
    while i + 1 < n {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(close) = body[i + 2..].find("]]").map(|p| i + 2 + p) {
                spans.push((i, close + 2));
                i = close + 2;
                continue;
            }
        }
        i += 1;
    }
    spans
}

fn is_inside_span(spans: &[(usize, usize)], pos: usize) -> bool {
    spans.iter().any(|(s, e)| pos >= *s && pos < *e)
}

fn scan(
    body: &str,
    entities: &EntityIndex,
    explicit_spans: &[(usize, usize)],
) -> Vec<Detection> {
    let body_lower = body.to_ascii_lowercase();
    let mut hits: Vec<Detection> = Vec::new();
    let mut covered: Vec<(usize, usize)> = Vec::new();

    for (form, entity_id) in &entities.forms_sorted {
        let mut search_from = 0;
        while let Some(found_in_slice) = body_lower[search_from..].find(form.as_str()) {
            let start = search_from + found_in_slice;
            let end = start + form.len();
            search_from = end;

            // Skip if this position is inside an explicit wiki-link.
            if is_inside_span(explicit_spans, start) {
                continue;
            }
            // Skip if a longer form already covered this region.
            if covered.iter().any(|(s, e)| start < *e && end > *s) {
                continue;
            }
            // Require word boundaries: the char before/after must not be
            // alphanumeric. Otherwise "evan" matches "evangelist".
            if !is_word_boundary_before(body, start)
                || !is_word_boundary_after(body, end)
            {
                continue;
            }

            let matched_text = body[start..end].to_owned();
            covered.push((start, end));
            hits.push(Detection {
                entity_id: *entity_id,
                start_offset: start,
                end_offset: end,
                matched_text,
            });
        }
    }

    // Sort by offset for stable downstream display.
    hits.sort_by_key(|d| d.start_offset);
    // Keep at most one suggestion per (entity_id, exact-offset) — defensive,
    // shouldn't trigger given the `covered` check above.
    let mut seen: Vec<(i64, usize)> = Vec::new();
    hits.retain(|d| {
        let key = (d.entity_id, d.start_offset);
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    hits
}

fn is_word_boundary_before(body: &str, pos: usize) -> bool {
    if pos == 0 {
        return true;
    }
    body.as_bytes()
        .get(pos - 1)
        .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_')
}

fn is_word_boundary_after(body: &str, pos: usize) -> bool {
    body.as_bytes()
        .get(pos)
        .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_')
}

/// Promote a pending `name_match` to `user_confirmed`. Returns the number
/// of rows updated (0 if the mention has already been removed).
pub fn confirm(
    project: &mut Project,
    mention_id: i64,
) -> Result<usize> {
    let n = project.database().conn().execute(
        "UPDATE entity_mentions
            SET source = 'user_confirmed', confidence = 1.0
          WHERE id = ?1 AND source = 'name_match'",
        params![mention_id],
    )?;
    Ok(n)
}

/// Reject and delete a pending `name_match`.
pub fn reject(project: &mut Project, mention_id: i64) -> Result<usize> {
    let n = project.database().conn().execute(
        "DELETE FROM entity_mentions
          WHERE id = ?1 AND source = 'name_match'",
        params![mention_id],
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(entries: &[(&str, i64)]) -> EntityIndex {
        let mut forms_sorted: Vec<(String, i64)> = entries
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        forms_sorted.sort_by_key(|(form, _)| std::cmp::Reverse(form.len()));
        EntityIndex { forms_sorted }
    }

    #[test]
    fn longest_form_wins_over_shorter_alias() {
        let entities = idx(&[("evan calder", 1), ("evan", 1)]);
        let body = "Evan Calder walked in.";
        let hits = scan(body, &entities, &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].matched_text, "Evan Calder");
    }

    #[test]
    fn match_inside_wiki_link_is_skipped() {
        let entities = idx(&[("evan", 1)]);
        let body = "[[Evan Calder]] talked to Evan again.";
        let explicit = collect_explicit_spans(body);
        let hits = scan(body, &entities, &explicit);
        // Should detect the second Evan, not the one inside [[…]].
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].matched_text, "Evan");
        assert!(hits[0].start_offset > 10);
    }

    #[test]
    fn requires_word_boundaries() {
        let entities = idx(&[("evan", 1)]);
        let body = "evangelist evan-something evanish";
        let hits = scan(body, &entities, &[]);
        // "evan" inside "evangelist" and "evanish" should NOT match.
        // "evan-something" has a `-` boundary so it should match.
        assert!(hits
            .iter()
            .any(|h| h.matched_text.as_str() == "evan"));
        for hit in &hits {
            let after = body.as_bytes().get(hit.end_offset).copied();
            assert!(after.is_none() || !after.unwrap().is_ascii_alphanumeric());
        }
    }

    #[test]
    fn case_insensitive_match() {
        let entities = idx(&[("aletheia", 1)]);
        let body = "Then ALETHEIA spoke; Aletheia agreed; aletheia laughed.";
        let hits = scan(body, &entities, &[]);
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn no_overlap_in_results() {
        let entities = idx(&[("evan calder", 1), ("calder", 2)]);
        let body = "Evan Calder walked past Calder Bridge.";
        let hits = scan(body, &entities, &[]);
        // Should match "Evan Calder" once and "Calder" inside "Calder
        // Bridge" once, but the "Calder" inside "Evan Calder" must NOT
        // count.
        assert_eq!(hits.len(), 2);
        let ranges: Vec<(usize, usize)> =
            hits.iter().map(|h| (h.start_offset, h.end_offset)).collect();
        for (i, a) in ranges.iter().enumerate() {
            for b in &ranges[i + 1..] {
                let overlap = a.0 < b.1 && b.0 < a.1;
                assert!(!overlap, "ranges should not overlap: {a:?} vs {b:?}");
            }
        }
    }
}
