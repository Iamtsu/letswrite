-- letswrite v1 schema.
--
-- Markdown files on disk are the source of truth for prose. This database is
-- an index/cache living at <project_root>/.letswrite/db.sqlite. It should be
-- safe to delete and rebuild from the on-disk Markdown at any time.
--
-- Conventions:
--   * Times are ISO-8601 strings (TEXT) in UTC, so they sort lexicographically
--     and survive JSON round-trips without surprise.
--   * Foreign keys cascade on document/project deletion: removing the parent
--     drops its dependent index rows.
--   * Enum-like columns use CHECK constraints rather than a separate table.
--     Keeps the schema flat and lets sqlite optimize without joins.
--   * JSON blobs hold open-ended data (frontmatter, entity payload, aliases).
--     Indexable fields are promoted to columns; everything else stays JSON.

PRAGMA foreign_keys = ON;

-- ---------------------------------------------------------------------------
-- projects: one per opened directory. Currently the DB is per-project, so
-- this table usually has exactly one row — but modeling it explicitly keeps
-- foreign keys consistent and leaves room for a future workspace view.
-- ---------------------------------------------------------------------------
CREATE TABLE projects (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    root_path   TEXT NOT NULL UNIQUE,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- ---------------------------------------------------------------------------
-- documents: one row per Markdown file. `rel_path` is relative to the
-- project root and uses forward slashes regardless of OS.
-- `kind` is the document's role in the project, derived from its folder
-- (chapter, scene, idea, character, location, meta, research).
-- `frontmatter_json` caches the parsed YAML frontmatter so views don't need
-- to re-parse the file on every render.
-- ---------------------------------------------------------------------------
CREATE TABLE documents (
    id                  INTEGER PRIMARY KEY,
    project_id          INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    rel_path            TEXT NOT NULL,
    kind                TEXT NOT NULL CHECK (kind IN (
        'chapter', 'scene', 'idea', 'character', 'location', 'meta', 'research'
    )),
    title               TEXT NOT NULL,
    frontmatter_json    TEXT NOT NULL DEFAULT '{}',
    body_hash           TEXT,
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (project_id, rel_path)
);

CREATE INDEX documents_project_kind ON documents (project_id, kind);

-- ---------------------------------------------------------------------------
-- entities: named things that recur across documents — characters, locations,
-- factions, items, concepts. The primary editable representation is usually a
-- document (Characters/<Name>.md), tied here via `document_id`.
-- `aliases_json`: ["Evan", "Calder"] — string array, used for mention matching.
-- `data_json`: open-ended structured fields (motivation timeline, traits, …).
-- ---------------------------------------------------------------------------
CREATE TABLE entities (
    id              INTEGER PRIMARY KEY,
    project_id      INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    document_id     INTEGER REFERENCES documents(id) ON DELETE SET NULL,
    kind            TEXT NOT NULL CHECK (kind IN (
        'character', 'location', 'faction', 'item', 'concept'
    )),
    name            TEXT NOT NULL,
    aliases_json    TEXT NOT NULL DEFAULT '[]',
    data_json       TEXT NOT NULL DEFAULT '{}',
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (project_id, kind, name)
);

CREATE INDEX entities_project_kind ON entities (project_id, kind);
CREATE INDEX entities_document ON entities (document_id);

-- ---------------------------------------------------------------------------
-- entity_mentions: links a span in a document to an entity. `source`
-- distinguishes how the mention was added so the UI can show confidence:
--   explicit_tag   — [[wiki-link]] in the prose
--   name_match     — heuristic name match (needs user confirmation)
--   ai_suggested   — proposed by the assistant
--   user_confirmed — promoted from a suggestion by the user
-- Offsets are byte positions in the *body* (after frontmatter is stripped).
-- ---------------------------------------------------------------------------
CREATE TABLE entity_mentions (
    id              INTEGER PRIMARY KEY,
    document_id     INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    entity_id       INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    start_offset    INTEGER NOT NULL CHECK (start_offset >= 0),
    end_offset      INTEGER NOT NULL CHECK (end_offset > start_offset),
    source          TEXT NOT NULL CHECK (source IN (
        'explicit_tag', 'name_match', 'ai_suggested', 'user_confirmed'
    )),
    confidence      REAL NOT NULL DEFAULT 1.0 CHECK (confidence >= 0.0 AND confidence <= 1.0)
);

CREATE INDEX entity_mentions_document ON entity_mentions (document_id);
CREATE INDEX entity_mentions_entity ON entity_mentions (entity_id);

-- ---------------------------------------------------------------------------
-- scenes: parsed from chapter documents on `## Beat N: Title` boundaries.
-- `order_index` is the scene's position within its parent document
-- (gap-friendly: scenes can be reordered without renumbering everything).
-- `when_in_story` is a free-text story-time marker (e.g. "Day 1, morning")
-- so authors can express in-fiction ordering without a strict timestamp.
-- ---------------------------------------------------------------------------
CREATE TABLE scenes (
    id                  INTEGER PRIMARY KEY,
    document_id         INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    order_index         REAL NOT NULL,
    synopsis            TEXT NOT NULL DEFAULT '',
    status              TEXT NOT NULL DEFAULT 'draft' CHECK (status IN (
        'draft', 'revised', 'final'
    )),
    pov_entity_id       INTEGER REFERENCES entities(id) ON DELETE SET NULL,
    location_entity_id  INTEGER REFERENCES entities(id) ON DELETE SET NULL,
    when_in_story       TEXT,
    start_offset        INTEGER NOT NULL CHECK (start_offset >= 0),
    end_offset          INTEGER NOT NULL CHECK (end_offset > start_offset)
);

CREATE INDEX scenes_document_order ON scenes (document_id, order_index);

-- ---------------------------------------------------------------------------
-- relationships: directed edges between entities. `kind` is a label
-- (knows, allied-with, opposed-to, romantic, professional, related-to, …) —
-- not constrained at the schema level so authors can invent labels.
-- `since_scene_id` anchors when the relationship begins in story time.
-- ---------------------------------------------------------------------------
CREATE TABLE relationships (
    id              INTEGER PRIMARY KEY,
    from_entity_id  INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    to_entity_id    INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL,
    since_scene_id  INTEGER REFERENCES scenes(id) ON DELETE SET NULL,
    notes           TEXT NOT NULL DEFAULT '',
    UNIQUE (from_entity_id, to_entity_id, kind)
);

CREATE INDEX relationships_from ON relationships (from_entity_id);
CREATE INDEX relationships_to ON relationships (to_entity_id);

-- ---------------------------------------------------------------------------
-- timeline_entries: per-entity evolution. Lets a character's motivation in
-- chapter 3 differ from chapter 20 without losing the chapter-3 version.
-- `field` is the structured field name being tracked (motivation, role,
-- emotional_state, …). `value` is free text.
-- ---------------------------------------------------------------------------
CREATE TABLE timeline_entries (
    id          INTEGER PRIMARY KEY,
    entity_id   INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    scene_id    INTEGER REFERENCES scenes(id) ON DELETE SET NULL,
    field       TEXT NOT NULL,
    value       TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX timeline_entries_entity_field ON timeline_entries (entity_id, field);

-- ---------------------------------------------------------------------------
-- snapshots: per-document content versions for "save before I rewrite".
-- Not full git. `content_blob_path` points at a file under
-- <project_root>/.letswrite/snapshots/, content-addressed by SHA-256.
-- ---------------------------------------------------------------------------
CREATE TABLE snapshots (
    id                  INTEGER PRIMARY KEY,
    document_id         INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    label               TEXT NOT NULL DEFAULT '',
    content_blob_path   TEXT NOT NULL,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX snapshots_document ON snapshots (document_id, created_at);

-- ---------------------------------------------------------------------------
-- goals: word count goals by scope. `scope` says what `scope_ref` refers to:
--   project | chapter | session | daily
-- For 'chapter', scope_ref is the document id (as text). For 'session' and
-- 'daily', scope_ref is the YYYY-MM-DD date string.
-- ---------------------------------------------------------------------------
CREATE TABLE goals (
    id              INTEGER PRIMARY KEY,
    project_id      INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    scope           TEXT NOT NULL CHECK (scope IN ('project', 'chapter', 'session', 'daily')),
    scope_ref       TEXT NOT NULL DEFAULT '',
    target_words    INTEGER NOT NULL CHECK (target_words > 0),
    target_date     TEXT,
    UNIQUE (project_id, scope, scope_ref)
);

-- ---------------------------------------------------------------------------
-- ai_threads: persisted assistant conversations. One thread per (document,
-- thread_name) so users can keep multiple parallel critique conversations
-- on the same document. `messages_json` is the full Vec<Message> from the
-- AI abstraction; we don't normalize it because the message shape is owned
-- by letswrite-ai and is expected to evolve.
-- ---------------------------------------------------------------------------
CREATE TABLE ai_threads (
    id              INTEGER PRIMARY KEY,
    project_id      INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    document_id     INTEGER REFERENCES documents(id) ON DELETE SET NULL,
    thread_name     TEXT NOT NULL DEFAULT 'default',
    messages_json   TEXT NOT NULL DEFAULT '[]',
    token_usage     INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (project_id, document_id, thread_name)
);

CREATE INDEX ai_threads_document ON ai_threads (document_id);
