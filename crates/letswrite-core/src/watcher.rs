//! Filesystem watcher that emits high-level [`WatchEvent`]s for the project.
//!
//! Wraps `notify` via `notify-debouncer-full` so rapid bursts (editors that
//! write via swap-then-rename, IDE autosave flurries) coalesce into a single
//! logical change per file. Events are scoped to Markdown files inside the
//! known project folders; everything else (including the `.letswrite/`
//! sidecar) is filtered out.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};

use crate::document::DocumentKind;
use crate::error::{Error, Result};

/// High-level event derived from filesystem activity.
///
/// The watcher already filters to relevant Markdown files; consumers can pass
/// each path to [`crate::project::Project::sync_path`] without further filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// File was created, modified, or appeared (e.g. via rename-into).
    Touched(PathBuf),
    /// File was removed.
    Removed(PathBuf),
}

/// Active filesystem watcher. Drop the value to stop watching.
pub struct ProjectWatcher {
    _debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
    rx: Receiver<WatchEvent>,
}

impl std::fmt::Debug for ProjectWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectWatcher").finish_non_exhaustive()
    }
}

impl ProjectWatcher {
    /// Start watching `project_root`. Events for files outside the known
    /// project folders (Chapters, Characters, ...) are dropped; events
    /// inside `.letswrite/` are dropped too.
    pub fn start(project_root: impl Into<PathBuf>) -> Result<Self> {
        Self::start_with_debounce(project_root, Duration::from_millis(250))
    }

    /// Same as [`Self::start`] but with a configurable debounce window
    /// (primarily for tests, which want to keep total runtime short).
    pub fn start_with_debounce(
        project_root: impl Into<PathBuf>,
        debounce: Duration,
    ) -> Result<Self> {
        let project_root = project_root.into();
        if !project_root.is_dir() {
            return Err(Error::InvalidData(format!(
                "{} is not a directory",
                project_root.display()
            )));
        }
        let (tx, rx) = mpsc::channel::<WatchEvent>();
        let root_for_handler = project_root.clone();
        let mut debouncer = new_debouncer(
            debounce,
            None,
            move |res: DebounceEventResult| match res {
                Ok(events) => {
                    for ev in events {
                        for path in &ev.paths {
                            if !relevant(&root_for_handler, path) {
                                continue;
                            }
                            let we = match ev.event.kind {
                                EventKind::Remove(_) => WatchEvent::Removed(path.clone()),
                                _ => WatchEvent::Touched(path.clone()),
                            };
                            // If the consumer dropped the receiver, the watcher
                            // is effectively detached — log once at debug and
                            // stop trying. There's no recovery from this.
                            if tx.send(we).is_err() {
                                tracing::debug!("watcher receiver dropped");
                                return;
                            }
                        }
                    }
                }
                Err(errs) => {
                    for err in errs {
                        tracing::warn!(?err, "filesystem watcher error");
                    }
                }
            },
        )
        .map_err(|e| Error::InvalidData(format!("could not start watcher: {e}")))?;

        debouncer
            .watch(&project_root, RecursiveMode::Recursive)
            .map_err(|e| Error::InvalidData(format!("could not watch root: {e}")))?;

        Ok(Self { _debouncer: debouncer, rx })
    }

    /// Block until the next event arrives, or return `None` if the watcher
    /// has shut down. Primarily for tests; UI consumers should use the
    /// non-blocking `try_recv` form below.
    pub fn recv(&self) -> Option<WatchEvent> {
        self.rx.recv().ok()
    }

    pub fn try_recv(&self) -> Option<WatchEvent> {
        self.rx.try_recv().ok()
    }

    /// Block for at most `timeout`, then return whatever's queued.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<WatchEvent> {
        self.rx.recv_timeout(timeout).ok()
    }
}

/// Is this path one the watcher should report? Markdown files inside one of
/// the known project folders, excluding `.letswrite/` and tempfiles.
fn relevant(project_root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(project_root) else {
        return false;
    };
    if rel.components().next().is_none() {
        return false;
    }
    // Skip the sidecar.
    if rel.starts_with(crate::project::LETSWRITE_DIR) {
        return false;
    }
    // Must be inside a known kind folder.
    if DocumentKind::from_rel_path(rel).is_none() {
        return false;
    }
    // Must look like Markdown and not be a temp file or hidden file.
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    if name.starts_with('.') || name.ends_with(".md.tmp") {
        return false;
    }
    Path::new(name).extension().and_then(|s| s.to_str()) == Some("md")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, DocumentKind};
    use crate::project::Project;
    use std::thread;
    use tempfile::tempdir;

    /// Drain events for up to `total` time. Returns whatever was emitted.
    fn drain(w: &ProjectWatcher, total: Duration) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        let deadline = std::time::Instant::now() + total;
        while std::time::Instant::now() < deadline {
            if let Some(ev) = w.recv_timeout(Duration::from_millis(100)) {
                out.push(ev);
            }
        }
        out
    }

    #[test]
    fn reports_markdown_changes_inside_known_folders() {
        let dir = tempdir().unwrap();
        let _ = Project::init(dir.path(), "P").unwrap();

        let watcher = ProjectWatcher::start_with_debounce(
            dir.path(),
            Duration::from_millis(80),
        )
        .unwrap();

        // Give the watcher a moment to settle on platforms that emit
        // initial scan events.
        thread::sleep(Duration::from_millis(150));
        let _initial = drain(&watcher, Duration::from_millis(100));

        Document::new(
            "Chapters/Chapter 1.md",
            Some(DocumentKind::Chapter),
            "Chapter 1",
            "hi",
        )
        .save(dir.path())
        .unwrap();

        let events = drain(&watcher, Duration::from_millis(1500));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, WatchEvent::Touched(p) if p.ends_with("Chapter 1.md"))),
            "expected a Touched event for Chapter 1.md, got: {events:?}"
        );
    }

    #[test]
    fn ignores_files_outside_known_folders_and_in_letswrite() {
        let dir = tempdir().unwrap();
        let _ = Project::init(dir.path(), "P").unwrap();

        let watcher = ProjectWatcher::start_with_debounce(
            dir.path(),
            Duration::from_millis(80),
        )
        .unwrap();
        thread::sleep(Duration::from_millis(150));
        let _initial = drain(&watcher, Duration::from_millis(100));

        // Outside any known folder.
        std::fs::create_dir_all(dir.path().join("ScratchNotes")).unwrap();
        std::fs::write(dir.path().join("ScratchNotes/x.md"), "stray").unwrap();
        // Inside the sidecar.
        std::fs::write(dir.path().join(".letswrite/marker"), "x").unwrap();

        let events = drain(&watcher, Duration::from_millis(800));
        assert!(
            events.is_empty(),
            "no events expected for unknown folders or sidecar, got: {events:?}"
        );
    }
}
