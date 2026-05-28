//! End-to-end tests for the Markdown files importer.

use std::fs;
use std::path::Path;

use letswrite_core::Project;
use letswrite_import::{confirm, detect_for_document, import_project, reject, ImportReport};
use rusqlite::params;
use tempfile::TempDir;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn count(project: &Project, sql: &str, p: i64) -> i64 {
    project
        .database()
        .conn()
        .query_row(sql, params![p], |r| r.get(0))
        .unwrap()
}

fn seed_minimal_threshold(root: &Path) {
    write(
        root,
        "Characters/Evan Calder.md",
        "---\ntitle: Evan Calder\ntype: character\naliases:\n  - Evan\n  - Calder\n---\n# Evan\nDeputy director.\n",
    );
    write(
        root,
        "Characters/Aletheia.md",
        "---\ntitle: Aletheia\ntype: character\naliases:\n  - Alex\n---\n# Aletheia\n",
    );
    write(
        root,
        "Locations/Strategic Integrity Unit.md",
        "---\ntitle: Strategic Integrity Unit\ntype: location\n---\nGovernment office.\n",
    );
    write(
        root,
        "Chapters/Chapter 2 - The Ghost File.md",
        "# Chapter 2\n\n## Beat 1: The Fog\n\n[[Evan]] drove through fog.\n\n## Beat 2: The Summons\n\n[[Susan]] called him to [[Strategic Integrity Unit]].\n",
    );
}

fn init_project(dir: &TempDir) -> Project {
    let mut p = Project::init(dir.path(), "The Threshold").unwrap();
    seed_minimal_threshold(dir.path());
    p.reindex().unwrap();
    p
}

#[test]
fn import_populates_entities_scenes_and_mentions() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = init_project(&dir);
    let report = import_project(&mut p).unwrap();

    assert_eq!(report.characters, 2, "Evan + Aletheia");
    assert_eq!(report.locations, 1);
    assert_eq!(report.scenes, 2);
    // 2 resolved mentions: [[Evan]] (via alias) and [[Strategic Integrity Unit]] (by name).
    assert_eq!(report.mentions, 2);
    // 1 unresolved: [[Susan]] doesn't have an entity file yet.
    assert_eq!(report.unresolved_mentions, 1);

    let pid = p.id();
    assert_eq!(count(&p, "SELECT COUNT(*) FROM entities WHERE project_id = ?1", pid), 3);
    assert_eq!(count(&p, "SELECT COUNT(*) FROM scenes \
        WHERE document_id IN (SELECT id FROM documents WHERE project_id = ?1)", pid), 2);
    assert_eq!(count(&p, "SELECT COUNT(*) FROM entity_mentions \
        WHERE document_id IN (SELECT id FROM documents WHERE project_id = ?1)", pid), 2);
}

#[test]
fn import_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = init_project(&dir);
    let r1 = import_project(&mut p).unwrap();
    let r2 = import_project(&mut p).unwrap();
    assert_eq!(r1, r2);

    // Counts shouldn't double after a second run — wipe-and-rebuild
    // guarantees a stable state.
    let pid = p.id();
    assert_eq!(count(&p, "SELECT COUNT(*) FROM entities WHERE project_id = ?1", pid), 3);
    assert_eq!(count(&p, "SELECT COUNT(*) FROM entity_mentions \
        WHERE document_id IN (SELECT id FROM documents WHERE project_id = ?1)", pid), 2);
}

#[test]
fn aliases_resolve_to_entity() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = init_project(&dir);
    let _ = import_project(&mut p).unwrap();

    // [[Evan]] (alias) and [[Calder]] (alias) and [[Evan Calder]] (name) all
    // map to the same entity.
    write(
        dir.path(),
        "Ideas/Misc.md",
        "Saw [[Evan]] and [[Calder]] and [[Evan Calder]] today.\n",
    );
    p.reindex().unwrap();
    let _ = import_project(&mut p).unwrap();

    let evan_id: i64 = p
        .database()
        .conn()
        .query_row(
            "SELECT id FROM entities WHERE project_id = ?1 AND name = 'Evan Calder'",
            params![p.id()],
            |r| r.get(0),
        )
        .unwrap();
    let mentions_of_evan: i64 = p
        .database()
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM entity_mentions em \
              JOIN documents d ON d.id = em.document_id \
              WHERE em.entity_id = ?1 AND d.rel_path = 'Ideas/Misc.md'",
            params![evan_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mentions_of_evan, 3, "all three alias forms resolve to Evan");
}

#[test]
fn rerun_after_renaming_an_entity_does_not_orphan_mentions() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = init_project(&dir);
    let _ = import_project(&mut p).unwrap();

    // Rename Aletheia.md to a new name; old mentions of [[Aletheia]] should
    // become unresolved (and disappear) on re-import, not point at stale rows.
    let old = dir.path().join("Characters/Aletheia.md");
    let new = dir.path().join("Characters/Athena.md");
    let body = fs::read_to_string(&old).unwrap().replace("Aletheia", "Athena");
    fs::remove_file(&old).unwrap();
    fs::write(&new, body).unwrap();
    write(
        dir.path(),
        "Ideas/Note.md",
        "wave to [[Athena]] and [[Aletheia]]\n",
    );
    p.reindex().unwrap();
    let report = import_project(&mut p).unwrap();

    // [[Athena]] resolves, [[Aletheia]] does not.
    let pid = p.id();
    let names: Vec<String> = p
        .database()
        .conn()
        .prepare("SELECT name FROM entities WHERE project_id = ?1 ORDER BY name")
        .unwrap()
        .query_map(params![pid], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(names.contains(&"Athena".to_owned()));
    assert!(!names.contains(&"Aletheia".to_owned()));
    assert!(report.unresolved_mentions >= 1);
}

#[test]
fn reject_persists_across_redetection() {
    // After a user rejects a name_match, re-running the detector on the
    // same document must not re-create the suggestion. This was the
    // original "suggestions reappear after every keystroke" bug.
    let dir = tempfile::tempdir().unwrap();
    let mut p = init_project(&dir);
    let _ = import_project(&mut p).unwrap();

    // A chapter where "Evan" appears in prose (no wiki-link), so the
    // detector will find it as a name_match suggestion.
    write(dir.path(), "Chapters/Draft.md", "Evan walked into the office.\n");
    p.reindex().unwrap();
    let abs = dir.path().join("Chapters/Draft.md");

    let inserted = detect_for_document(&mut p, &abs).unwrap();
    assert_eq!(inserted, 1, "first scan finds the name_match");

    let mention_id: i64 = p
        .database()
        .conn()
        .query_row(
            "SELECT em.id FROM entity_mentions em
                JOIN documents d ON d.id = em.document_id
                WHERE d.rel_path = 'Chapters/Draft.md' AND em.source = 'name_match'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    reject(&mut p, mention_id).unwrap();

    let inserted_again = detect_for_document(&mut p, &abs).unwrap();
    assert_eq!(
        inserted_again, 0,
        "rejection is remembered; the detector skips this name"
    );
}

#[test]
fn confirm_returns_action_with_aliased_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = init_project(&dir);
    let _ = import_project(&mut p).unwrap();

    // Two matches: one alias ("Evan") and one canonical ("Evan Calder").
    // The alias should be confirmed with the colon-suffix; the canonical
    // name should drop the redundant suffix.
    write(
        dir.path(),
        "Chapters/Draft.md",
        "Evan walked into the office. Later, Evan Calder spoke.\n",
    );
    p.reindex().unwrap();
    let abs = dir.path().join("Chapters/Draft.md");
    detect_for_document(&mut p, &abs).unwrap();

    let mention_ids: Vec<(i64, String)> = p
        .database()
        .conn()
        .prepare(
            "SELECT em.id, em.matched_text FROM entity_mentions em
                JOIN documents d ON d.id = em.document_id
                WHERE d.rel_path = 'Chapters/Draft.md' AND em.source = 'name_match'
                ORDER BY em.start_offset",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(mention_ids.len(), 2);
    let (alias_id, _) = mention_ids
        .iter()
        .find(|(_, m)| m == "Evan")
        .expect("alias mention exists")
        .clone();
    let (canon_id, _) = mention_ids
        .iter()
        .find(|(_, m)| m == "Evan Calder")
        .expect("canonical mention exists")
        .clone();

    let alias_action = confirm(&mut p, alias_id)
        .unwrap()
        .expect("pending mention exists");
    assert_eq!(alias_action.rel_path, "Chapters/Draft.md");
    assert_eq!(
        alias_action.replacement, "[[Evan Calder: Evan]]",
        "alias keeps the colon suffix"
    );
    let body = fs::read_to_string(&abs).unwrap();
    assert_eq!(
        &body[alias_action.start_offset..alias_action.end_offset],
        "Evan"
    );

    let canon_action = confirm(&mut p, canon_id)
        .unwrap()
        .expect("pending mention exists");
    assert_eq!(
        canon_action.replacement, "[[Evan Calder]]",
        "canonical name skips the redundant alias suffix"
    );

    // Confirm deleted both name_match rows.
    let remaining: i64 = p
        .database()
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM entity_mentions
                WHERE id IN (?1, ?2)",
            params![alias_id, canon_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0, "confirm dropped both name_match rows");
}

#[test]
fn import_handles_real_threshold_vault_if_present() {
    // Optional smoke test against the dogfood novel. Skips quietly if the
    // vault isn't there (CI, fresh clones).
    let real = Path::new("/home/tsu/Projects/private/The-Threshold");
    if !real.is_dir() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path();
    // Copy the relevant folders. We don't want to mutate the real vault.
    for sub in ["Chapters", "Characters", "Locations", "Ideas", "Meta", "Research"] {
        let src = real.join(sub);
        if !src.exists() {
            continue;
        }
        copy_dir(&src, &dest.join(sub));
    }
    let mut p = Project::open(dest).unwrap();
    p.reindex().unwrap();
    let report = import_project(&mut p).unwrap();
    // Loose assertions — vault evolves over time; we only check it parsed.
    assert!(report.characters >= 1, "at least one character expected");
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&from, &to);
        } else if kind.is_file() {
            fs::copy(&from, &to).unwrap();
        }
    }
}

// Compile-time check that ImportReport is constructible — guards against
// the struct fields drifting silently.
#[allow(dead_code)]
fn _report_compiles() -> ImportReport {
    ImportReport::default()
}
