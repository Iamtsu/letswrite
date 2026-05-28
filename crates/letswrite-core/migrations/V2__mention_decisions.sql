-- V2: track user decisions on entity-mention suggestions so the detector
-- doesn't re-emit the same name_match every time the document is saved.
--
-- Two additions:
--
-- 1. `entity_mentions.matched_text` — the exact prose span that produced
--    this mention. Stored at detection time when byte offsets are still
--    fresh. Used by the Confirm-from-suggestion flow to splice
--    `[[Entity: matched]]` into the prose, and as the join key for
--    matching against `rejected_mentions`.
--
-- 2. `rejected_mentions` — a per-document deny-list. `reject` inserts a
--    row here; on the next scan the detector skips any hit whose
--    (document_id, entity_id, matched_text_lower) is already present.
--    `matched_text_lower` is stored case-normalised so a rejection of
--    "Mara" also covers later occurrences of "mara".

ALTER TABLE entity_mentions ADD COLUMN matched_text TEXT NOT NULL DEFAULT '';

CREATE TABLE rejected_mentions (
    id                  INTEGER PRIMARY KEY,
    document_id         INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    entity_id           INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    matched_text_lower  TEXT NOT NULL,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (document_id, entity_id, matched_text_lower)
);

CREATE INDEX rejected_mentions_document ON rejected_mentions (document_id);
