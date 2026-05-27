//! SQLite-backed project index.
//!
//! Lives at `<project_root>/.letswrite/db.sqlite` and is treated as a cache:
//! the on-disk Markdown is the source of truth and the database can be
//! deleted and rebuilt from it at any time.
//!
//! Migrations live in `crates/letswrite-core/migrations/` and are bundled
//! into the binary at compile time by [`refinery::embed_migrations!`].

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{Error, Result};

mod embedded {
    refinery::embed_migrations!("migrations");
}

/// Handle to an open project database. Wraps a `rusqlite::Connection` with
/// the pragmas and migrations applied.
pub struct Database {
    conn: Connection,
    path: PathBuf,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Connection` doesn't implement Debug, so we only show what's safe
        // to print; `finish_non_exhaustive` makes this explicit.
        f.debug_struct("Database").field("path", &self.path).finish_non_exhaustive()
    }
}

impl Database {
    /// Open (or create) the database at `path`, apply pragmas, and run
    /// pending migrations. The parent directory is created if missing.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| Error::io_at(parent, e))?;
            }
        }
        let mut conn = Connection::open(&path)?;
        apply_pragmas(&conn)?;
        // refinery uses `&mut Connection` so it can run the migrations in a
        // transaction and write to its bookkeeping table.
        let report = embedded::migrations::runner().run(&mut conn)?;
        if !report.applied_migrations().is_empty() {
            tracing::info!(
                count = report.applied_migrations().len(),
                target = report.applied_migrations().last().map(refinery::Migration::version),
                "applied database migrations"
            );
        }
        Ok(Self { conn, path })
    }

    /// Open an in-memory database — primarily for tests. Migrations are
    /// applied so the schema matches a real on-disk database.
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        apply_pragmas(&conn)?;
        embedded::migrations::runner().run(&mut conn)?;
        Ok(Self { conn, path: PathBuf::from(":memory:") })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn conn(&self) -> &Connection {
        &self.conn
    }

    pub const fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

fn apply_pragmas(conn: &Connection) -> Result<()> {
    // WAL: better concurrency for the watcher/UI; durable enough for our
    // index-cache use case (we can rebuild from Markdown if it corrupts).
    // foreign_keys: enforced per-connection, not stored in the DB header.
    // synchronous=NORMAL is the WAL-recommended setting.
    // busy_timeout: tolerate the file watcher racing with the UI thread.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn assert_table_exists(db: &Database, name: &str) {
        let count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
                [name],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "table {name} should exist after migrations");
    }

    #[test]
    fn open_creates_file_and_applies_schema() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sub").join("db.sqlite");
        let db = Database::open(&path).unwrap();
        assert!(path.exists(), "db file should have been created");
        for table in [
            "projects",
            "documents",
            "entities",
            "entity_mentions",
            "scenes",
            "relationships",
            "timeline_entries",
            "snapshots",
            "goals",
            "ai_threads",
        ] {
            assert_table_exists(&db, table);
        }
    }

    #[test]
    fn in_memory_db_runs_migrations() {
        let db = Database::open_in_memory().unwrap();
        assert_table_exists(&db, "documents");
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let _ = Database::open(&path).unwrap();
        let _ = Database::open(&path).unwrap();
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let db = Database::open_in_memory().unwrap();
        // Inserting a document for a non-existent project must fail.
        let err = db.conn().execute(
            "INSERT INTO documents (project_id, rel_path, kind, title)
             VALUES (?1, ?2, ?3, ?4)",
            (999_i64, "Chapter 1.md", "chapter", "Chapter 1"),
        );
        assert!(err.is_err(), "FK violation should be rejected");
    }

    #[test]
    fn project_cascade_removes_documents() {
        let mut db = Database::open_in_memory().unwrap();
        let tx = db.conn_mut().transaction().unwrap();
        tx.execute(
            "INSERT INTO projects (id, name, root_path) VALUES (1, 'p', '/tmp/p')",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO documents (project_id, rel_path, kind, title)
             VALUES (1, 'a.md', 'chapter', 'A')",
            [],
        )
        .unwrap();
        tx.execute("DELETE FROM projects WHERE id = 1", []).unwrap();
        let doc_count: i64 = tx
            .query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(doc_count, 0, "cascade should have removed the document");
        tx.commit().unwrap();
    }

    #[test]
    fn round_trip_each_entity_type() {
        let mut db = Database::open_in_memory().unwrap();
        let tx = db.conn_mut().transaction().unwrap();

        // project
        tx.execute(
            "INSERT INTO projects (id, name, root_path) VALUES (1, 'The Threshold', '/x')",
            [],
        )
        .unwrap();

        // document (chapter) — supports non-ASCII titles
        tx.execute(
            "INSERT INTO documents (id, project_id, rel_path, kind, title)
             VALUES (1, 1, 'Chapters/Chapter 2 — The Ghost File.md', 'chapter',
                     'Chapter 2 — The Ghost File')",
            [],
        )
        .unwrap();

        // entities of each kind
        for (id, kind, name) in [
            (1, "character", "Evan Calder"),
            (2, "location", "Strategic Integrity Unit"),
            (3, "faction", "Strategic Integrity Unit Leadership"),
            (4, "item", "The Ghost File"),
            (5, "concept", "Managed harm"),
        ] {
            tx.execute(
                "INSERT INTO entities (id, project_id, kind, name)
                 VALUES (?1, 1, ?2, ?3)",
                (id, kind, name),
            )
            .unwrap();
        }

        // entity_mention
        tx.execute(
            "INSERT INTO entity_mentions
                 (document_id, entity_id, start_offset, end_offset, source)
             VALUES (1, 1, 0, 11, 'explicit_tag')",
            [],
        )
        .unwrap();

        // scene
        tx.execute(
            "INSERT INTO scenes (id, document_id, order_index, pov_entity_id,
                                  location_entity_id, start_offset, end_offset)
             VALUES (1, 1, 1.0, 1, 2, 0, 100)",
            [],
        )
        .unwrap();

        // relationship
        tx.execute(
            "INSERT INTO relationships (from_entity_id, to_entity_id, kind, since_scene_id)
             VALUES (1, 3, 'professional', 1)",
            [],
        )
        .unwrap();

        // timeline_entry
        tx.execute(
            "INSERT INTO timeline_entries (entity_id, scene_id, field, value)
             VALUES (1, 1, 'motivation', 'absorb systemic blame, preserve continuity')",
            [],
        )
        .unwrap();

        // snapshot
        tx.execute(
            "INSERT INTO snapshots (document_id, label, content_blob_path)
             VALUES (1, 'before rewrite', '.letswrite/snapshots/abc123')",
            [],
        )
        .unwrap();

        // goal
        tx.execute(
            "INSERT INTO goals (project_id, scope, scope_ref, target_words, target_date)
             VALUES (1, 'chapter', '1', 3000, '2026-07-01')",
            [],
        )
        .unwrap();

        // ai_thread
        tx.execute(
            "INSERT INTO ai_threads (project_id, document_id, thread_name, messages_json)
             VALUES (1, 1, 'critique', '[]')",
            [],
        )
        .unwrap();

        tx.commit().unwrap();

        // Spot-check counts.
        let counts: Vec<(String, i64)> = [
            "projects",
            "documents",
            "entities",
            "entity_mentions",
            "scenes",
            "relationships",
            "timeline_entries",
            "snapshots",
            "goals",
            "ai_threads",
        ]
        .into_iter()
        .map(|t| {
            let n: i64 = db
                .conn()
                .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap();
            (t.to_string(), n)
        })
        .collect();

        assert_eq!(counts[0].1, 1, "projects");
        assert_eq!(counts[1].1, 1, "documents");
        assert_eq!(counts[2].1, 5, "entities (one per kind)");
        assert!(counts[3..].iter().all(|(_, n)| *n == 1));
    }

    #[test]
    fn check_constraints_reject_bad_enums() {
        let mut db = Database::open_in_memory().unwrap();
        let tx = db.conn_mut().transaction().unwrap();
        tx.execute(
            "INSERT INTO projects (id, name, root_path) VALUES (1, 'p', '/tmp/p')",
            [],
        )
        .unwrap();
        let err = tx.execute(
            "INSERT INTO documents (project_id, rel_path, kind, title)
             VALUES (1, 'a.md', 'novel', 'A')",
            [],
        );
        assert!(err.is_err(), "CHECK on documents.kind should reject 'novel'");
    }

    #[test]
    fn mention_offsets_validated() {
        let mut db = Database::open_in_memory().unwrap();
        let tx = db.conn_mut().transaction().unwrap();
        tx.execute(
            "INSERT INTO projects (id, name, root_path) VALUES (1, 'p', '/tmp/p')",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO documents (id, project_id, rel_path, kind, title)
             VALUES (1, 1, 'a.md', 'chapter', 'A')",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO entities (id, project_id, kind, name)
             VALUES (1, 1, 'character', 'X')",
            [],
        )
        .unwrap();
        let err = tx.execute(
            "INSERT INTO entity_mentions (document_id, entity_id, start_offset, end_offset, source)
             VALUES (1, 1, 10, 10, 'name_match')",
            [],
        );
        assert!(err.is_err(), "end_offset must be strictly greater than start_offset");
    }
}
