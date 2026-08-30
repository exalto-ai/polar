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

-- Which actor last touched each block, and which first wrote it.
--
-- Derived state, like `snapshots` and `doc_fts`: rebuildable by replaying the
-- op log, and dropped without loss. It exists because the question "who wrote
-- this paragraph" is asked per block, and answering it from the log every time
-- would mean replaying a document's whole history on every read.
--
-- Yjs cannot carry this (AD-1), which is what AD-6's insistence on identity
-- from the first commit was for.
CREATE TABLE IF NOT EXISTS block_provenance (
  doc_id      TEXT    NOT NULL REFERENCES documents(id),
  block_id    TEXT    NOT NULL,
  -- Kept apart on purpose: a paragraph an agent drafted and a human then
  -- reworded is *both*, and collapsing the two loses the half that explains
  -- where the text came from.
  created_by  TEXT    NOT NULL REFERENCES actors(id),
  created_at  INTEGER NOT NULL,
  touched_by  TEXT    NOT NULL REFERENCES actors(id),
  touched_at  INTEGER NOT NULL,
  -- The run that last touched it, so per-run revert (AD-11) has an anchor.
  session_id  TEXT,
  PRIMARY KEY (doc_id, block_id)
);
CREATE INDEX IF NOT EXISTS block_provenance_doc ON block_provenance(doc_id);

-- One compact event per durable mutation. This is local attribution metadata,
-- not a signature or a tamper-proof ledger.
CREATE TABLE IF NOT EXISTS provenance_events (
  event_id        INTEGER PRIMARY KEY,
  doc_id          TEXT    NOT NULL REFERENCES documents(id),
  update_seq      INTEGER NOT NULL UNIQUE REFERENCES updates(seq),
  actor_id        TEXT    REFERENCES actors(id),
  action          TEXT    NOT NULL CHECK (action IN ('edit', 'trash', 'restore')),
  group_key       TEXT    NOT NULL,
  source_label    TEXT    NOT NULL,
  ingress         TEXT    NOT NULL CHECK (ingress IN (
                    'entered', 'command', 'pasted', 'imported', 'mcp', 'api',
                    'suggestion', 'unknown', 'legacy_unknown'
                  )),
  assurance       TEXT    NOT NULL CHECK (assurance IN (
                    'observed', 'reported', 'verified', 'unknown'
                  )),
  alignment       TEXT    NOT NULL CHECK (alignment IN ('exact', 'inferred', 'unknown')),
  session_id      TEXT,
  created_at      INTEGER NOT NULL,
  UNIQUE (doc_id, event_id)
);
CREATE INDEX IF NOT EXISTS provenance_events_doc_event
  ON provenance_events(doc_id, event_id);

-- Authoritative current surviving text. It changes in the same transaction as
-- the Yjs update and provenance event, so no replay or cache digest is needed.
CREATE TABLE IF NOT EXISTS lineage_spans (
  doc_id          TEXT    NOT NULL REFERENCES documents(id),
  block_id        TEXT    NOT NULL,
  node_path       TEXT    NOT NULL,
  start_utf16     INTEGER NOT NULL CHECK (start_utf16 >= 0),
  end_utf16       INTEGER NOT NULL CHECK (end_utf16 > start_utf16),
  source_event_id INTEGER NOT NULL,
  PRIMARY KEY (doc_id, block_id, node_path, start_utf16),
  FOREIGN KEY (doc_id, source_event_id)
    REFERENCES provenance_events(doc_id, event_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS lineage_spans_source ON lineage_spans(source_event_id);

CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts USING fts5(
  doc_id UNINDEXED, title, body, tokenize='porter unicode61'
);

-- A reviewer credential authorizes one configured, read-only ingress. The raw
-- credential lives in an owner-only native file; SQLite keeps only its hash.
CREATE TABLE IF NOT EXISTS reviewer_connections (
  id              TEXT PRIMARY KEY CHECK (
                    length(id) BETWEEN 1 AND 64
                    AND id NOT GLOB '*[^a-z0-9-]*'
                  ),
  client          TEXT NOT NULL CHECK (client IN (
                    'chatgpt', 'codex', 'claude_desktop', 'claude_code'
                  )),
  display_label   TEXT NOT NULL CHECK (length(trim(display_label)) BETWEEN 1 AND 80),
  document_scope  TEXT NOT NULL CHECK (document_scope IN ('current', 'all')),
  document_id     TEXT REFERENCES documents(id),
  credential_hash BLOB NOT NULL UNIQUE CHECK (length(credential_hash) = 32),
  revision        INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  last_seen_at    INTEGER,
  revoked_at      INTEGER,
  reported_model  TEXT CHECK (reported_model IS NULL OR length(reported_model) <= 256),
  CHECK (
    (document_scope = 'current' AND document_id IS NOT NULL)
    OR (document_scope = 'all' AND document_id IS NULL)
  )
);
CREATE INDEX IF NOT EXISTS reviewer_connections_active
  ON reviewer_connections(revoked_at, updated_at DESC);
"#;
