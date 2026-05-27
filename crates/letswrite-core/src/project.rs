//! A `Project` is a directory on disk with a known layout and a `.letswrite/`
//! sidecar holding the `SQLite` index.
//!
//! Layout:
//! ```text
//! <project_root>/
//!     Chapters/
//!     Scenes/        (optional; many projects keep scenes inside Chapters/N/)
//!     Characters/
//!     Locations/
//!     Ideas/
//!     Meta/
//!     Research/
//!     .letswrite/
//!         db.sqlite
//!         snapshots/    (created on first use, see #9)
//! ```
//!
//! Markdown files inside the known folders are indexed into `SQLite`; files
//! outside them (or outside the project root) are ignored.

use std::path::{Path, PathBuf};

use rusqlite::params;
use walkdir::WalkDir;

use crate::db::Database;
use crate::document::{Document, DocumentKind};
use crate::error::{Error, Result};

pub(crate) const LETSWRITE_DIR: &str = ".letswrite";
const DB_FILENAME: &str = "db.sqlite";

/// A project opened on disk. Owns its database handle; closing the project
/// drops the connection.
#[derive(Debug)]
pub struct Project {
    root: PathBuf,
    db: Database,
    id: i64,
    name: String,
}

impl Project {
    /// Create the project layout in an empty (or non-existent) directory,
    /// then open it. Use [`Self::open`] for an existing project.
    pub fn init(root: impl Into<PathBuf>, name: impl Into<String>) -> Result<Self> {
        let root = root.into();
        let name = name.into();
        if !root.exists() {
            std::fs::create_dir_all(&root).map_err(|e| Error::io_at(&root, e))?;
        }
        if !root.is_dir() {
            return Err(Error::InvalidData(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        for kind in DocumentKind::ALL {
            let dir = root.join(kind.folder());
            if !dir.exists() {
                std::fs::create_dir_all(&dir).map_err(|e| Error::io_at(&dir, e))?;
            }
        }
        let letswrite = root.join(LETSWRITE_DIR);
        if !letswrite.exists() {
            std::fs::create_dir_all(&letswrite).map_err(|e| Error::io_at(&letswrite, e))?;
        }
        Self::open_with_name(root, Some(name))
    }

    /// Open an existing project. The `.letswrite/` directory and database
    /// are created if missing (so opening a plain Obsidian-style vault
    /// works the first time).
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_name(root.into(), None)
    }

    fn open_with_name(root: PathBuf, override_name: Option<String>) -> Result<Self> {
        if !root.is_dir() {
            return Err(Error::InvalidData(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        let letswrite = root.join(LETSWRITE_DIR);
        if !letswrite.exists() {
            std::fs::create_dir_all(&letswrite).map_err(|e| Error::io_at(&letswrite, e))?;
        }
        let db_path = letswrite.join(DB_FILENAME);
        let db = Database::open(&db_path)?;

        let root_str = root
            .to_str()
            .ok_or_else(|| Error::InvalidData("project root is not valid UTF-8".to_owned()))?
            .to_owned();
        let name = override_name.unwrap_or_else(|| {
            root.file_name()
                .and_then(|s| s.to_str())
                .map_or_else(|| root_str.clone(), str::to_owned)
        });

        // Upsert the project row. Each project DB usually has a single
        // project row; the UNIQUE(root_path) keeps it stable across opens.
        let project_id: i64 = {
            let tx = db.conn().unchecked_transaction()?;
            tx.execute(
                "INSERT INTO projects (name, root_path) VALUES (?1, ?2)
                 ON CONFLICT(root_path) DO UPDATE SET
                     name = excluded.name,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
                params![name, root_str],
            )?;
            let id =
                tx.query_row("SELECT id FROM projects WHERE root_path = ?1", [&root_str], |r| {
                    r.get(0)
                })?;
            tx.commit()?;
            id
        };

        Ok(Self { root, db, id: project_id, name })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn id(&self) -> i64 {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn database(&self) -> &Database {
        &self.db
    }

    pub const fn database_mut(&mut self) -> &mut Database {
        &mut self.db
    }

    /// Walk the project's known folders and yield every Markdown file as
    /// (absolute path, classified kind).
    pub fn scan(&self) -> Vec<ScannedFile> {
        let mut out = Vec::new();
        for kind in DocumentKind::ALL {
            let dir = self.root.join(kind.folder());
            if !dir.exists() {
                continue;
            }
            for entry in WalkDir::new(&dir).follow_links(false).into_iter().flatten() {
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.into_path();
                if path.extension().and_then(|s| s.to_str()) != Some("md") {
                    continue;
                }
                // Skip hidden files like `.DS_Store`-adjacent stuff and the
                // tempfiles produced by atomic writes.
                if path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.starts_with('.') || n.ends_with(".md.tmp"))
                {
                    continue;
                }
                out.push(ScannedFile { path, kind });
            }
        }
        out
    }

    /// Full reindex: walk the project, load every Markdown file, upsert
    /// each into `documents`. Returns the number of documents indexed.
    /// Removes index rows for documents whose file no longer exists.
    pub fn reindex(&mut self) -> Result<usize> {
        let files = self.scan();
        let project_id = self.id;
        let root = self.root.clone();
        let conn = self.db.conn_mut();
        let tx = conn.transaction()?;
        let mut seen_rel_paths: Vec<String> = Vec::with_capacity(files.len());
        for file in &files {
            let doc = Document::load(&root, &file.path)?;
            upsert_document(&tx, project_id, &doc)?;
            seen_rel_paths.push(doc.rel_path);
        }
        // Drop index rows for files that no longer exist on disk. Build the
        // delete with a NOT IN of a temp table to avoid a huge `?` list.
        tx.execute(
            "CREATE TEMP TABLE _seen (rel_path TEXT PRIMARY KEY) WITHOUT ROWID",
            [],
        )?;
        {
            let mut ins = tx.prepare("INSERT INTO _seen (rel_path) VALUES (?1)")?;
            for rel in &seen_rel_paths {
                ins.execute([rel])?;
            }
        }
        tx.execute(
            "DELETE FROM documents
              WHERE project_id = ?1
                AND rel_path NOT IN (SELECT rel_path FROM _seen)",
            params![project_id],
        )?;
        tx.execute("DROP TABLE _seen", [])?;
        tx.commit()?;
        Ok(seen_rel_paths.len())
    }

    /// Apply a single-file change. `abs_path` is the file that changed (or
    /// was removed). Used by the watcher to keep `SQLite` in sync.
    pub fn sync_path(&mut self, abs_path: &Path) -> Result<SyncOutcome> {
        if abs_path.exists() {
            // Treat as create-or-update.
            let doc = Document::load(&self.root, abs_path)?;
            let project_id = self.id;
            let conn = self.db.conn_mut();
            let tx = conn.transaction()?;
            upsert_document(&tx, project_id, &doc)?;
            tx.commit()?;
            Ok(SyncOutcome::Upserted)
        } else {
            // File no longer present — drop the index row if it exists.
            let rel = abs_path.strip_prefix(&self.root).map_err(|_| {
                Error::InvalidData(format!(
                    "{} is not inside project root {}",
                    abs_path.display(),
                    self.root.display()
                ))
            })?;
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let project_id = self.id;
            let removed = self.db.conn().execute(
                "DELETE FROM documents WHERE project_id = ?1 AND rel_path = ?2",
                params![project_id, rel_str],
            )?;
            Ok(if removed > 0 { SyncOutcome::Removed } else { SyncOutcome::NoOp })
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub kind: DocumentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    Upserted,
    Removed,
    NoOp,
}

fn upsert_document(
    tx: &rusqlite::Transaction<'_>,
    project_id: i64,
    doc: &Document,
) -> Result<()> {
    let Some(kind) = doc.kind else {
        // Outside the known folders — ignore silently.
        return Ok(());
    };
    let frontmatter_json = serde_json::to_string(&doc.frontmatter_json()?)?;
    let body_hash = doc.body_hash();
    tx.execute(
        "INSERT INTO documents
             (project_id, rel_path, kind, title, frontmatter_json, body_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(project_id, rel_path) DO UPDATE SET
             kind = excluded.kind,
             title = excluded.title,
             frontmatter_json = excluded.frontmatter_json,
             body_hash = excluded.body_hash,
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        params![
            project_id,
            doc.rel_path,
            kind.as_db_str(),
            doc.title,
            frontmatter_json,
            body_hash,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use tempfile::tempdir;

    fn count_docs(p: &Project) -> i64 {
        p.database()
            .conn()
            .query_row("SELECT COUNT(*) FROM documents WHERE project_id = ?1",
                params![p.id()], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn init_creates_layout_and_db() {
        let dir = tempdir().unwrap();
        let p = Project::init(dir.path(), "The Threshold").unwrap();
        for kind in DocumentKind::ALL {
            assert!(dir.path().join(kind.folder()).is_dir(), "{kind} folder");
        }
        assert!(dir.path().join(".letswrite/db.sqlite").is_file());
        assert_eq!(p.name(), "The Threshold");
        assert_eq!(count_docs(&p), 0);
    }

    #[test]
    fn open_is_idempotent_and_keeps_project_id_stable() {
        let dir = tempdir().unwrap();
        let id_first = Project::init(dir.path(), "P").unwrap().id();
        let id_second = Project::open(dir.path()).unwrap().id();
        assert_eq!(id_first, id_second, "project_id must stay stable across opens");
    }

    #[test]
    fn reindex_picks_up_known_folder_files_and_ignores_unknown() {
        let dir = tempdir().unwrap();
        let mut p = Project::init(dir.path(), "P").unwrap();

        Document::new(
            "Chapters/Chapter 1.md",
            Some(DocumentKind::Chapter),
            "Chapter 1",
            "body",
        )
        .save(dir.path())
        .unwrap();
        Document::new(
            "Characters/Evan.md",
            Some(DocumentKind::Character),
            "Evan",
            "bio",
        )
        .save(dir.path())
        .unwrap();
        // Unknown folder — should be ignored.
        std::fs::create_dir_all(dir.path().join("Random")).unwrap();
        std::fs::write(dir.path().join("Random/x.md"), "stray").unwrap();

        let n = p.reindex().unwrap();
        assert_eq!(n, 2);
        assert_eq!(count_docs(&p), 2);
    }

    #[test]
    fn reindex_prunes_deleted_files() {
        let dir = tempdir().unwrap();
        let mut p = Project::init(dir.path(), "P").unwrap();
        Document::new("Ideas/Idea A.md", Some(DocumentKind::Idea), "A", "x")
            .save(dir.path())
            .unwrap();
        p.reindex().unwrap();
        assert_eq!(count_docs(&p), 1);
        std::fs::remove_file(dir.path().join("Ideas/Idea A.md")).unwrap();
        p.reindex().unwrap();
        assert_eq!(count_docs(&p), 0);
    }

    #[test]
    fn sync_path_handles_create_update_and_delete() {
        let dir = tempdir().unwrap();
        let mut p = Project::init(dir.path(), "P").unwrap();
        let abs = dir.path().join("Chapters/Chapter 1.md");

        Document::new(
            "Chapters/Chapter 1.md",
            Some(DocumentKind::Chapter),
            "Chapter 1",
            "first version",
        )
        .save(dir.path())
        .unwrap();
        assert_eq!(p.sync_path(&abs).unwrap(), SyncOutcome::Upserted);
        assert_eq!(count_docs(&p), 1);

        // Update: body changes, body_hash must change.
        Document::new(
            "Chapters/Chapter 1.md",
            Some(DocumentKind::Chapter),
            "Chapter 1",
            "second version",
        )
        .save(dir.path())
        .unwrap();
        assert_eq!(p.sync_path(&abs).unwrap(), SyncOutcome::Upserted);
        let hashes: Vec<String> = p
            .database()
            .conn()
            .prepare("SELECT body_hash FROM documents")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(hashes.len(), 1);

        // Delete on disk → sync removes the row.
        std::fs::remove_file(&abs).unwrap();
        assert_eq!(p.sync_path(&abs).unwrap(), SyncOutcome::Removed);
        assert_eq!(count_docs(&p), 0);
    }

    #[test]
    fn frontmatter_and_title_stored_in_db() {
        let dir = tempdir().unwrap();
        let mut p = Project::init(dir.path(), "P").unwrap();
        let text = "---\ntitle: Evan Calder\nrole: protagonist\ntags:\n  - threshold\n---\n# Bio\n";
        std::fs::write(dir.path().join("Characters/Evan Calder.md"), text).unwrap();
        p.reindex().unwrap();
        let (title, fm_json): (String, String) = p
            .database()
            .conn()
            .query_row(
                "SELECT title, frontmatter_json FROM documents",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, "Evan Calder");
        let v: serde_json::Value = serde_json::from_str(&fm_json).unwrap();
        assert_eq!(v["role"], "protagonist");
        assert_eq!(v["tags"][0], "threshold");
    }
}
