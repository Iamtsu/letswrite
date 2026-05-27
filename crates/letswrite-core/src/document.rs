//! On-disk Markdown document model.
//!
//! Each [`Document`] corresponds to one `.md` file inside a project. The
//! file may optionally start with a YAML frontmatter block delimited by
//! `---` on its own lines; the body is everything after the closing
//! delimiter (or the entire file if no frontmatter is present).
//!
//! Round-trip is preserved for valid input: parsing then writing back the
//! same `Document` produces byte-identical output when the frontmatter
//! came in as a normal YAML mapping. Writing-from-scratch always uses a
//! canonical form (frontmatter first, trailing newline on the body).

use std::fmt;
use std::path::Path;

use serde_json::Value as JsonValue;
use serde_yaml::Value as YamlValue;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// What role a document plays inside a project. Derived from the top-level
/// folder it lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocumentKind {
    Chapter,
    Scene,
    Idea,
    Character,
    Location,
    Meta,
    Research,
}

impl DocumentKind {
    /// The on-disk folder name for this kind (always plural, matches the
    /// project layout in [`crate::project`]).
    pub const fn folder(self) -> &'static str {
        match self {
            Self::Chapter => "Chapters",
            Self::Scene => "Scenes",
            Self::Idea => "Ideas",
            Self::Character => "Characters",
            Self::Location => "Locations",
            Self::Meta => "Meta",
            Self::Research => "Research",
        }
    }

    /// The database string for this kind. Matches the CHECK constraint on
    /// `documents.kind` in the v1 schema.
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Chapter => "chapter",
            Self::Scene => "scene",
            Self::Idea => "idea",
            Self::Character => "character",
            Self::Location => "location",
            Self::Meta => "meta",
            Self::Research => "research",
        }
    }

    /// Classify a path by its first path component (relative to the project
    /// root). Anything outside the known folders → `None`.
    pub fn from_rel_path(rel: &Path) -> Option<Self> {
        let first = rel.components().next()?;
        let first_str = first.as_os_str().to_str()?;
        match first_str {
            "Chapters" => Some(Self::Chapter),
            "Scenes" => Some(Self::Scene),
            "Ideas" => Some(Self::Idea),
            "Characters" => Some(Self::Character),
            "Locations" => Some(Self::Location),
            "Meta" => Some(Self::Meta),
            "Research" => Some(Self::Research),
            _ => None,
        }
    }

    /// All kinds, in the same order as [`crate::project::Project::init`]
    /// creates the folders.
    pub const ALL: [Self; 7] = [
        Self::Chapter,
        Self::Scene,
        Self::Idea,
        Self::Character,
        Self::Location,
        Self::Meta,
        Self::Research,
    ];
}

impl fmt::Display for DocumentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_db_str())
    }
}

/// A parsed Markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Path relative to the project root, using forward slashes.
    pub rel_path: String,
    /// Classified role (folder-derived). `None` if the file is outside the
    /// known project folders — those are loaded but ignored by indexing.
    pub kind: Option<DocumentKind>,
    /// Display title: frontmatter `title` field if present, otherwise the
    /// file stem.
    pub title: String,
    /// Raw frontmatter as a YAML mapping. Empty mapping if no frontmatter
    /// block was present.
    pub frontmatter: YamlValue,
    /// Body text, excluding the frontmatter delimiters.
    pub body: String,
}

const FRONTMATTER_DELIMITER: &str = "---";

impl Document {
    /// Build a fresh document for writing. `body` should not contain
    /// frontmatter delimiters.
    pub fn new(
        rel_path: impl Into<String>,
        kind: Option<DocumentKind>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            rel_path: rel_path.into(),
            kind,
            title: title.into(),
            frontmatter: YamlValue::Mapping(serde_yaml::Mapping::default()),
            body: body.into(),
        }
    }

    /// Load a document from disk by its absolute path, given the project root
    /// it belongs to.
    pub fn load(project_root: &Path, abs_path: &Path) -> Result<Self> {
        let rel = abs_path.strip_prefix(project_root).map_err(|_| {
            Error::InvalidData(format!(
                "{} is not inside project root {}",
                abs_path.display(),
                project_root.display()
            ))
        })?;
        let rel_path = rel_path_to_string(rel);
        let text =
            std::fs::read_to_string(abs_path).map_err(|e| Error::io_at(abs_path, e))?;
        Self::from_text(rel_path, &text)
    }

    /// Parse a document from its raw text contents, given the relative path
    /// it would live at.
    pub fn from_text(rel_path: String, text: &str) -> Result<Self> {
        let (frontmatter, body) = split_frontmatter(text)?;
        let kind = DocumentKind::from_rel_path(Path::new(&rel_path));
        let title = derive_title(&rel_path, &frontmatter);
        Ok(Self { rel_path, kind, title, frontmatter, body: body.to_owned() })
    }

    /// Serialize back to a string. If the frontmatter mapping is non-empty,
    /// emit a frontmatter block; otherwise emit just the body.
    pub fn to_text(&self) -> Result<String> {
        let needs_frontmatter = match &self.frontmatter {
            YamlValue::Mapping(m) => !m.is_empty(),
            YamlValue::Null => false,
            _ => true,
        };
        if !needs_frontmatter {
            return Ok(self.body.clone());
        }
        let yaml = serde_yaml::to_string(&self.frontmatter)?;
        let mut out = String::with_capacity(yaml.len() + self.body.len() + 16);
        out.push_str(FRONTMATTER_DELIMITER);
        out.push('\n');
        out.push_str(yaml.trim_end_matches('\n'));
        out.push('\n');
        out.push_str(FRONTMATTER_DELIMITER);
        out.push('\n');
        out.push_str(&self.body);
        Ok(out)
    }

    /// Write to `project_root/<self.rel_path>` atomically (sibling tempfile
    /// + rename). Parent dirs are created if missing.
    pub fn save(&self, project_root: &Path) -> Result<()> {
        let abs = project_root.join(&self.rel_path);
        if let Some(parent) = abs.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| Error::io_at(parent, e))?;
            }
        }
        let text = self.to_text()?;
        atomic_write(&abs, &text)
    }

    /// SHA-256 of the body, hex-encoded. Used as the `documents.body_hash`
    /// column to detect changes without reparsing.
    pub fn body_hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.body.as_bytes());
        format!("{:x}", h.finalize())
    }

    /// Frontmatter as a JSON value, for storage in `documents.frontmatter_json`.
    pub fn frontmatter_json(&self) -> Result<JsonValue> {
        yaml_to_json(&self.frontmatter)
    }
}

fn rel_path_to_string(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn split_frontmatter(text: &str) -> Result<(YamlValue, &str)> {
    // A frontmatter block requires `---\n` on the very first line. Without
    // that, the entire text is body.
    let Some(after_open) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return Ok((YamlValue::Mapping(serde_yaml::Mapping::default()), text));
    };
    // Find the next line that consists solely of `---`. We require it to be
    // followed by either end-of-text, `\n`, or `\r\n`.
    let mut search_from = 0;
    while let Some(idx) = after_open[search_from..].find("---") {
        let abs = search_from + idx;
        let is_line_start = abs == 0 || after_open.as_bytes()[abs - 1] == b'\n';
        let after = &after_open[abs + 3..];
        let line_ends = after.is_empty()
            || after.starts_with('\n')
            || after.starts_with("\r\n");
        if is_line_start && line_ends {
            let raw = &after_open[..abs];
            let frontmatter: YamlValue = if raw.trim().is_empty() {
                YamlValue::Mapping(serde_yaml::Mapping::default())
            } else {
                serde_yaml::from_str(raw).map_err(|e| {
                    Error::InvalidFrontmatter(format!("could not parse YAML: {e}"))
                })?
            };
            // Skip past the closing delimiter line. The closing newline can be
            // `\r\n`, `\n`, or absent (end of file).
            let newline_len = if after.starts_with("\r\n") {
                2
            } else {
                usize::from(after.starts_with('\n'))
            };
            return Ok((frontmatter, &after_open[abs + 3 + newline_len..]));
        }
        search_from = abs + 3;
    }
    Err(Error::InvalidFrontmatter(
        "opening `---` without a matching closing `---`".to_owned(),
    ))
}

fn derive_title(rel_path: &str, frontmatter: &YamlValue) -> String {
    if let YamlValue::Mapping(m) = frontmatter {
        if let Some(YamlValue::String(s)) = m.get(YamlValue::String("title".to_owned())) {
            return s.clone();
        }
    }
    Path::new(rel_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map_or_else(|| rel_path.to_owned(), str::to_owned)
}

fn yaml_number_to_json(n: &serde_yaml::Number) -> JsonValue {
    // Three-way ladder is genuinely clearer than nesting map_or_else.
    #[allow(clippy::option_if_let_else)]
    if let Some(i) = n.as_i64() {
        JsonValue::Number(i.into())
    } else if let Some(u) = n.as_u64() {
        JsonValue::Number(u.into())
    } else if let Some(f) = n.as_f64() {
        serde_json::Number::from_f64(f).map_or(JsonValue::Null, JsonValue::Number)
    } else {
        JsonValue::Null
    }
}

/// Convert a `serde_yaml::Value` to `serde_json::Value`. YAML mapping keys
/// that aren't strings become their YAML serialization (rare; defensive).
fn yaml_to_json(v: &YamlValue) -> Result<JsonValue> {
    Ok(match v {
        YamlValue::Null => JsonValue::Null,
        YamlValue::Bool(b) => JsonValue::Bool(*b),
        YamlValue::Number(n) => yaml_number_to_json(n),
        YamlValue::String(s) => JsonValue::String(s.clone()),
        YamlValue::Sequence(seq) => {
            JsonValue::Array(seq.iter().map(yaml_to_json).collect::<Result<_>>()?)
        }
        YamlValue::Mapping(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                let key = match k {
                    YamlValue::String(s) => s.clone(),
                    other => serde_yaml::to_string(other)
                        .unwrap_or_default()
                        .trim()
                        .to_owned(),
                };
                obj.insert(key, yaml_to_json(v)?);
            }
            JsonValue::Object(obj)
        }
        YamlValue::Tagged(t) => yaml_to_json(&t.value)?,
    })
}

/// Atomic write via sibling tempfile + rename. Same pattern as
/// [`crate::settings::Settings::save_to`].
fn atomic_write(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension({
        let mut ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned();
        ext.push_str(".tmp");
        ext
    });
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| Error::io_at(&tmp, e))?;
        f.write_all(text.as_bytes()).map_err(|e| Error::io_at(&tmp, e))?;
        f.sync_all().map_err(|e| Error::io_at(&tmp, e))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| Error::io_at(path, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_document_without_frontmatter() {
        let doc = Document::from_text(
            "Chapters/Chapter 1.md".to_owned(),
            "Just the body.\n",
        )
        .unwrap();
        assert!(matches!(doc.frontmatter, YamlValue::Mapping(ref m) if m.is_empty()));
        assert_eq!(doc.body, "Just the body.\n");
        assert_eq!(doc.kind, Some(DocumentKind::Chapter));
        assert_eq!(doc.title, "Chapter 1");
    }

    #[test]
    fn parses_document_with_frontmatter() {
        let text = "---\ntitle: Evan Calder\ntype: character\n---\n# Evan\n\nBody here.\n";
        let doc =
            Document::from_text("Characters/Evan Calder.md".to_owned(), text).unwrap();
        assert_eq!(doc.title, "Evan Calder");
        assert_eq!(doc.kind, Some(DocumentKind::Character));
        assert!(doc.body.starts_with("# Evan"));
    }

    #[test]
    fn handles_crlf_line_endings() {
        let text = "---\r\ntitle: T\r\n---\r\nbody\r\n";
        let doc = Document::from_text("Meta/T.md".to_owned(), text).unwrap();
        assert_eq!(doc.title, "T");
        assert_eq!(doc.body, "body\r\n");
    }

    #[test]
    fn body_with_triple_dash_inside_is_safe() {
        // A `---` inside the body but not at column 0 of a line shouldn't
        // be treated as a closing frontmatter delimiter.
        let text = "---\ntitle: t\n---\nintro --- still body\n---\nfooter\n";
        let doc = Document::from_text("Meta/T.md".to_owned(), text).unwrap();
        assert_eq!(doc.title, "t");
        assert!(doc.body.contains("intro --- still body"));
        assert!(doc.body.contains("footer"));
    }

    #[test]
    fn missing_closing_delimiter_errors() {
        let err =
            Document::from_text("Meta/T.md".to_owned(), "---\ntitle: t\nno closing\n")
                .unwrap_err();
        assert!(matches!(err, Error::InvalidFrontmatter(_)));
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempdir().unwrap();
        let doc = Document::new(
            "Characters/Evan Calder.md",
            Some(DocumentKind::Character),
            "Evan Calder",
            "# Evan\n\nDeputy director.\n",
        );
        // Push something into frontmatter so it actually serializes.
        let mut m = serde_yaml::Mapping::new();
        m.insert(
            YamlValue::String("title".into()),
            YamlValue::String("Evan Calder".into()),
        );
        m.insert(
            YamlValue::String("role".into()),
            YamlValue::String("protagonist".into()),
        );
        let doc = Document {
            frontmatter: YamlValue::Mapping(m),
            ..doc
        };
        doc.save(dir.path()).unwrap();
        let loaded = Document::load(
            dir.path(),
            &dir.path().join("Characters/Evan Calder.md"),
        )
        .unwrap();
        assert_eq!(loaded.title, "Evan Calder");
        assert_eq!(loaded.kind, Some(DocumentKind::Character));
        assert!(loaded.body.contains("Deputy director"));
        // Frontmatter mapping survived.
        if let YamlValue::Mapping(m) = &loaded.frontmatter {
            assert_eq!(
                m.get(YamlValue::String("role".into())),
                Some(&YamlValue::String("protagonist".into()))
            );
        } else {
            panic!("frontmatter should be a mapping");
        }
    }

    #[test]
    fn classifies_kind_from_folder() {
        assert_eq!(
            DocumentKind::from_rel_path(Path::new("Chapters/Chapter 2/x.md")),
            Some(DocumentKind::Chapter)
        );
        assert_eq!(
            DocumentKind::from_rel_path(Path::new("Characters/Evan.md")),
            Some(DocumentKind::Character)
        );
        assert_eq!(
            DocumentKind::from_rel_path(Path::new("Notes/random.md")),
            None
        );
    }

    #[test]
    fn empty_frontmatter_round_trip() {
        let text = "---\n---\nbody only\n";
        let doc = Document::from_text("Meta/T.md".to_owned(), text).unwrap();
        // An explicit empty frontmatter is treated as empty mapping.
        assert!(matches!(doc.frontmatter, YamlValue::Mapping(ref m) if m.is_empty()));
        // Re-serializing drops the empty block.
        assert_eq!(doc.to_text().unwrap(), "body only\n");
    }

    #[test]
    fn body_hash_is_deterministic_and_body_only() {
        let a = Document::new("a.md", None, "a", "body");
        let b = Document::new("b.md", None, "b", "body");
        assert_eq!(a.body_hash(), b.body_hash());
        let c = Document::new("c.md", None, "c", "body!");
        assert_ne!(a.body_hash(), c.body_hash());
    }

    #[test]
    fn frontmatter_to_json_preserves_strings_and_arrays() {
        let text = "---\ntitle: t\ntags:\n  - one\n  - two\nstatus: canonical\n---\nbody\n";
        let doc = Document::from_text("Meta/T.md".to_owned(), text).unwrap();
        let json = doc.frontmatter_json().unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj["title"], "t");
        assert_eq!(obj["status"], "canonical");
        let tags = obj["tags"].as_array().unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0], "one");
    }
}
