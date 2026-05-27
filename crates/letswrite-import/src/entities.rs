//! Derive `entities` rows from Character and Location documents.
//!
//! The entity's name comes from frontmatter `title` if present, otherwise
//! the file stem. Aliases come from frontmatter `aliases` (an array of
//! strings), and the rest of the frontmatter is preserved verbatim in
//! `data_json` for downstream views.

use std::path::PathBuf;

use rusqlite::params;
use serde_yaml::Value as YamlValue;

use letswrite_core::{Document, DocumentKind, Result};

#[allow(clippy::redundant_pub_crate)]
pub(crate) struct Outcome {
    pub characters: usize,
    pub locations: usize,
}

#[allow(clippy::redundant_pub_crate)]
pub(crate) fn import(
    tx: &rusqlite::Transaction<'_>,
    project_id: i64,
    docs: &[(DocumentKind, PathBuf, Document)],
) -> Result<Outcome> {
    let mut characters = 0;
    let mut locations = 0;

    for (kind, _, doc) in docs {
        let entity_kind = match kind {
            DocumentKind::Character => "character",
            DocumentKind::Location => "location",
            _ => continue,
        };

        let document_id = super::document_id(tx, project_id, &doc.rel_path)?;
        let name = entity_name(doc);
        let aliases = extract_aliases(&doc.frontmatter);
        let aliases_json = serde_json::to_string(&aliases)?;
        let data_json = serde_json::to_string(&doc.frontmatter_json()?)?;

        tx.execute(
            "INSERT INTO entities
                (project_id, document_id, kind, name, aliases_json, data_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id, kind, name) DO UPDATE SET
                document_id  = excluded.document_id,
                aliases_json = excluded.aliases_json,
                data_json    = excluded.data_json,
                updated_at   = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                project_id,
                document_id,
                entity_kind,
                name,
                aliases_json,
                data_json,
            ],
        )?;

        match kind {
            DocumentKind::Character => characters += 1,
            DocumentKind::Location => locations += 1,
            _ => unreachable!(),
        }
    }

    Ok(Outcome { characters, locations })
}

/// Resolve the entity's display name: prefer frontmatter `title`, then the
/// file stem (derived in `Document::load`).
fn entity_name(doc: &Document) -> String {
    if let YamlValue::Mapping(m) = &doc.frontmatter {
        if let Some(YamlValue::String(s)) = m.get(YamlValue::String("title".into())) {
            return s.clone();
        }
    }
    doc.title.clone()
}

/// Read `aliases:` from frontmatter as a list of strings. Tolerant of
/// scalar / null / missing, all of which return an empty list. Non-string
/// items inside a sequence are stringified via YAML serialization.
fn extract_aliases(frontmatter: &YamlValue) -> Vec<String> {
    let YamlValue::Mapping(m) = frontmatter else {
        return Vec::new();
    };
    let Some(value) = m.get(YamlValue::String("aliases".into())) else {
        return Vec::new();
    };
    match value {
        YamlValue::Sequence(items) => items
            .iter()
            .filter_map(|v| match v {
                YamlValue::String(s) => Some(s.clone()),
                YamlValue::Null => None,
                other => serde_yaml::to_string(other)
                    .ok()
                    .map(|s| s.trim().to_owned()),
            })
            .filter(|s| !s.is_empty())
            .collect(),
        YamlValue::String(s) => vec![s.clone()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml::Mapping;

    fn fm_with(pairs: &[(&str, YamlValue)]) -> YamlValue {
        let mut m = Mapping::new();
        for (k, v) in pairs {
            m.insert(YamlValue::String((*k).to_owned()), v.clone());
        }
        YamlValue::Mapping(m)
    }

    #[test]
    fn aliases_sequence_extracted() {
        let fm = fm_with(&[(
            "aliases",
            YamlValue::Sequence(vec![
                YamlValue::String("Evan".into()),
                YamlValue::String("Calder".into()),
            ]),
        )]);
        assert_eq!(extract_aliases(&fm), vec!["Evan", "Calder"]);
    }

    #[test]
    fn aliases_string_promoted_to_singleton() {
        let fm = fm_with(&[("aliases", YamlValue::String("Alex".into()))]);
        assert_eq!(extract_aliases(&fm), vec!["Alex"]);
    }

    #[test]
    fn aliases_missing_returns_empty() {
        assert_eq!(extract_aliases(&YamlValue::Null), Vec::<String>::new());
        assert_eq!(extract_aliases(&fm_with(&[])), Vec::<String>::new());
    }
}
