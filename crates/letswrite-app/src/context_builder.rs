//! Build an [`AssistantContext`] from the live app state.
//!
//! The UI is in the best position to know what the user is editing right now
//! — cursor position, selection, the document open — but it should not be
//! in the business of *choosing* what context to send to the LLM. The
//! builder here translates app state into the structured bundle defined in
//! `letswrite-ai`; the agent then decides what to do with it.

use std::path::PathBuf;

use letswrite_ai::{
    AssistantContext, ContextWindow, DocumentContext, EntityInScene, ProjectMeta, WindowKind,
};
use letswrite_core::{Document, Project};
use rusqlite::params;
use serde_yaml::Value as YamlValue;

use crate::editor::EditorSnapshot;

/// Inputs the builder needs from the rest of the app.
pub(crate) struct BuildInputs<'a> {
    pub project: Option<&'a Project>,
    pub project_root: Option<&'a std::path::Path>,
    pub editor: EditorSnapshot,
    /// Token budget for the context portion. Rough; the agent gets to
    /// be more accurate downstream.
    pub token_budget: u32,
}

/// Build the context. Failures (file vanished, DB unreachable) downgrade
/// gracefully — we'd rather send less context than crash the assistant.
pub(crate) fn build(inputs: &BuildInputs<'_>) -> AssistantContext {
    let document = inputs
        .project_root
        .zip(inputs.editor.rel_path.as_deref())
        .and_then(|(root, rel)| read_document_context(root, rel, &inputs.editor));

    let entities_in_scene = inputs
        .project
        .as_ref()
        .and_then(|p| {
            let rel = inputs.editor.rel_path.as_deref()?;
            Some(query_entities_in_document(p, rel))
        })
        .unwrap_or_default();

    let project_meta = inputs.project.map(load_project_meta).unwrap_or_default();
    let language = detect_language(document.as_ref());

    AssistantContext {
        selection: inputs.editor.selection.clone().filter(|s| !s.trim().is_empty()),
        document,
        entities_in_scene,
        project_meta,
        language,
        token_budget: inputs.token_budget,
    }
}

fn read_document_context(
    project_root: &std::path::Path,
    rel_path: &str,
    snapshot: &EditorSnapshot,
) -> Option<DocumentContext> {
    let abs = project_root.join(rel_path);
    let doc = Document::load(project_root, &abs).ok()?;
    let body = doc.body.clone();
    let cursor_offset = cursor_offset_from_snapshot(&body, snapshot);
    let window = pick_window(&body, cursor_offset);
    Some(DocumentContext {
        abs_path: abs,
        rel_path: doc.rel_path,
        title: doc.title,
        body,
        cursor_offset,
        window,
    })
}

/// Iced's `text_editor` reports cursor as `(line, column)`. Translate that
/// back into a byte offset into the body.
fn cursor_offset_from_snapshot(body: &str, snapshot: &EditorSnapshot) -> usize {
    let mut offset = 0;
    for (line_idx, line) in body.split_inclusive('\n').enumerate() {
        if line_idx == snapshot.cursor_line {
            // Add the column, clamped to the visible line length.
            let line_len = line.trim_end_matches('\n').len();
            let col = snapshot.cursor_column.min(line_len);
            return offset + col;
        }
        offset += line.len();
    }
    body.len()
}

/// Pick a context window around the cursor. Strategy: prefer the
/// scene the cursor is in (between two `## Beat …` headings); fall back
/// to the chapter; final fallback is the whole document.
fn pick_window(body: &str, cursor: usize) -> ContextWindow {
    // Locate the scene by scanning for `## Beat` headings on each side.
    let cursor = cursor.min(body.len());
    let prev = find_prev_beat_heading(body, cursor);
    let next = find_next_beat_heading(body, cursor);
    if let Some(start) = prev {
        let end = next.unwrap_or(body.len());
        return ContextWindow {
            kind: WindowKind::Scene,
            start,
            end,
        };
    }
    // No beat heading nearby — return the whole document.
    ContextWindow {
        kind: WindowKind::WholeDocument,
        start: 0,
        end: body.len(),
    }
}

fn find_prev_beat_heading(body: &str, cursor: usize) -> Option<usize> {
    // Walk lines from the start; keep the latest beat-heading whose start
    // is at or before the cursor.
    let mut latest: Option<usize> = None;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        if offset > cursor {
            break;
        }
        let line_no_nl = line.trim_end_matches('\n').trim_end_matches('\r');
        if is_beat_heading(line_no_nl) {
            latest = Some(offset);
        }
        offset += line.len();
    }
    latest
}

fn find_next_beat_heading(body: &str, cursor: usize) -> Option<usize> {
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let line_no_nl = line.trim_end_matches('\n').trim_end_matches('\r');
        if offset > cursor && is_beat_heading(line_no_nl) {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

fn is_beat_heading(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("## ") else {
        return false;
    };
    let lower = rest.to_ascii_lowercase();
    lower.starts_with("beat ") || lower.starts_with("beat:")
}

/// Query the `entity_mentions` joined to entities for everything that
/// occurs in the given document. The state field is best-effort for v1
/// — we surface the entity's frontmatter `data_json` blob as JSON for
/// the agent to interpret. A proper timeline-aware lookup lands later.
fn query_entities_in_document(project: &Project, rel_path: &str) -> Vec<EntityInScene> {
    let project_id = project.id();
    let conn = project.database().conn();
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT e.name, e.kind, e.data_json
           FROM entity_mentions em
           JOIN entities e ON e.id = em.entity_id
           JOIN documents d ON d.id = em.document_id
          WHERE d.project_id = ?1 AND d.rel_path = ?2
          ORDER BY e.name",
    ) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "entities-in-scene query failed to prepare");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(params![project_id, rel_path], |row| {
        let name: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let data_json: String = row.get(2)?;
        Ok((name, kind, data_json))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(%err, "entities-in-scene query failed");
            return Vec::new();
        }
    };
    rows.flatten()
        .map(|(name, kind, data_json)| EntityInScene {
            name,
            kind,
            current_state: summarise_entity_state(&data_json),
        })
        .collect()
}

/// Compress an entity's frontmatter JSON into a short one-liner the LLM
/// can scan quickly. Pulls out common fields by name; falls back to a
/// pretty-printed JSON for unknown shapes.
fn summarise_entity_state(data_json: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data_json) else {
        return "(no data)".to_owned();
    };
    let Some(obj) = value.as_object() else {
        return value.to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    for key in ["role", "function", "affiliation", "status", "blind_spot", "arc"] {
        if let Some(v) = obj.get(key) {
            if let Some(s) = v.as_str() {
                parts.push(format!("{key}: {s}"));
            }
        }
    }
    if parts.is_empty() {
        // Bail out: return the full JSON, length-limited.
        let mut s = value.to_string();
        if s.len() > 280 {
            s.truncate(277);
            s.push('…');
        }
        s
    } else {
        parts.join("; ")
    }
}

fn load_project_meta(project: &Project) -> Vec<ProjectMeta> {
    let root = project.root().to_path_buf();
    let mut metas = Vec::new();
    // Conventional files we recognise. Adding more = adding to this list.
    for (label, rel) in [
        ("Writing Guide", "Meta/Writing Guide.md"),
        ("Author's Statement", "Meta/Author's Statement.md"),
    ] {
        let path = root.join(rel);
        if path.is_file() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                metas.push(ProjectMeta {
                    label: label.to_owned(),
                    content: strip_frontmatter(&text),
                });
            }
        }
    }
    metas
}

/// Cheap frontmatter stripper for the meta-loader; doesn't need full
/// `Document::load` semantics.
fn strip_frontmatter(text: &str) -> String {
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return text.to_owned();
    };
    if let Some(close) = rest.find("\n---\n").or_else(|| rest.find("\r\n---\r\n")) {
        let after = &rest[close..];
        let after = after.trim_start_matches("\n---\n").trim_start_matches("\r\n---\r\n");
        return after.to_owned();
    }
    text.to_owned()
}

fn detect_language(doc: Option<&DocumentContext>) -> Option<unic_langid::LanguageIdentifier> {
    let doc = doc?;
    // First try frontmatter on the live document.
    let parsed = letswrite_core::Document::from_text(doc.rel_path.clone(), &doc.body).ok()?;
    if let YamlValue::Mapping(m) = &parsed.frontmatter {
        if let Some(YamlValue::String(lang)) = m.get(YamlValue::String("language".into())) {
            if let Ok(id) = lang.parse() {
                return Some(id);
            }
        }
    }
    None
}

// Unused PathBuf import dead-code guard; suppress when document is None.
#[allow(dead_code)]
fn _force_pathbuf_use(_: PathBuf) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_scene_window_when_cursor_is_inside_a_beat() {
        let body = "preamble\n\n## Beat 1: A\n\nbeat one body\n\n## Beat 2: B\n\nbeat two body\n";
        let cursor = body.find("beat one").unwrap();
        let window = pick_window(body, cursor);
        assert_eq!(window.kind, WindowKind::Scene);
        let snippet = &body[window.start..window.end];
        assert!(snippet.starts_with("## Beat 1"));
        assert!(snippet.contains("beat one"));
        assert!(!snippet.contains("Beat 2"));
    }

    #[test]
    fn falls_back_to_whole_document_when_no_beats() {
        let body = "# Chapter Title\n\nflat prose, no beats here.\n";
        let window = pick_window(body, 10);
        assert_eq!(window.kind, WindowKind::WholeDocument);
        assert_eq!(window.start, 0);
        assert_eq!(window.end, body.len());
    }

    #[test]
    fn cursor_offset_matches_for_simple_position() {
        let body = "line one\nline two\nline three\n";
        let snap = EditorSnapshot {
            cursor_line: 1,
            cursor_column: 5,
            ..EditorSnapshot::default()
        };
        let off = cursor_offset_from_snapshot(body, &snap);
        // "line one\n" = 9 bytes; "line " = 5 bytes; → 14
        assert_eq!(off, 14);
    }

    #[test]
    fn frontmatter_stripper_handles_lf_and_crlf() {
        let lf = "---\ntitle: t\n---\nbody\n";
        assert_eq!(strip_frontmatter(lf).trim(), "body");
        let crlf = "---\r\ntitle: t\r\n---\r\nbody\r\n";
        assert!(strip_frontmatter(crlf).contains("body"));
    }

    #[test]
    fn summarise_entity_uses_known_fields() {
        let data = r#"{"role":"protagonist","arc":"absorbs blame","unknown":"ignored"}"#;
        let s = summarise_entity_state(data);
        assert!(s.contains("role: protagonist"));
        assert!(s.contains("arc: absorbs blame"));
    }

    #[test]
    fn summarise_entity_falls_back_when_no_known_keys() {
        let data = r#"{"mystery":"thing"}"#;
        let s = summarise_entity_state(data);
        assert!(s.contains("mystery"));
    }
}
