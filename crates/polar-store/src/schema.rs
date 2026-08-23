//! The store's DDL, kept verbatim so it can be diffed against the ADR.

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS documents (
  id          TEXT PRIMARY KEY,
  title       TEXT NOT NULL DEFAULT '',
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL,
  -- Derived cache of the tombstone in the document CRDT (AD-14). A column
  -- cannot replicate, so this is never the source of truth.
  deleted_at  INTEGER,
  share_id    TEXT,
  relay_url   TEXT
);

-- Append-only. The source of truth, and never compacted (AD-13).
CREATE TABLE IF NOT EXISTS updates (
  seq         INTEGER PRIMARY KEY AUTOINCREMENT,
  doc_id      TEXT    NOT NULL REFERENCES documents(id),
  payload     BLOB    NOT NULL,
  actor_id    TEXT    NOT NULL REFERENCES actors(id),
  origin      TEXT    NOT NULL,
  session_id  TEXT,
  created_at  INTEGER NOT NULL,
  synced_at   INTEGER
);
CREATE INDEX IF NOT EXISTS updates_doc_seq  ON updates(doc_id, seq);
CREATE INDEX IF NOT EXISTS updates_unsynced ON updates(doc_id) WHERE synced_at IS NULL;

-- Load performance only. Discardable.
CREATE TABLE IF NOT EXISTS snapshots (
  doc_id       TEXT    NOT NULL REFERENCES documents(id),
  through_seq  INTEGER NOT NULL,
  state        BLOB    NOT NULL,
  state_vector BLOB    NOT NULL,
  created_at   INTEGER NOT NULL,
  PRIMARY KEY (doc_id, through_seq)
);

-- Self-asserted; this is identity, not authentication (AD-6).
CREATE TABLE IF NOT EXISTS actors (
  id           TEXT PRIMARY KEY,
  kind         TEXT NOT NULL,
  display_name TEXT NOT NULL,
  model        TEXT,
  color        TEXT NOT NULL,
  first_seen   INTEGER NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts USING fts5(
  doc_id UNINDEXED, title, body, tokenize='porter unicode61'
);
"#;
