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

CREATE TABLE IF NOT EXISTS reviewer_connections (
  id                      TEXT PRIMARY KEY CHECK (
                            length(id) BETWEEN 1 AND 64
                            AND id NOT GLOB '*[^a-z0-9-]*'
                          ),
  client                  TEXT NOT NULL CHECK (client IN (
                            'chatgpt', 'codex', 'claude_desktop', 'claude_code'
                          )),
  provider                TEXT NOT NULL CHECK (provider IN ('openai', 'anthropic')),
  display_label           TEXT NOT NULL CHECK (
                            length(trim(display_label)) BETWEEN 1 AND 80
                          ),
  status                  TEXT NOT NULL CHECK (status IN (
                            'configured', 'connected', 'disconnected', 'failed', 'revoked'
                          )),
  document_scope          TEXT NOT NULL CHECK (document_scope IN ('selected', 'all')),
  can_read                INTEGER NOT NULL DEFAULT 1 CHECK (can_read = 1),
  can_edit                INTEGER NOT NULL DEFAULT 1 CHECK (can_edit IN (0, 1)),
  can_create              INTEGER NOT NULL DEFAULT 0 CHECK (can_create IN (0, 1)),
  can_trash               INTEGER NOT NULL DEFAULT 0 CHECK (can_trash IN (0, 1)),
  credential_hash         BLOB NOT NULL UNIQUE CHECK (length(credential_hash) = 32),
  pending_credential_hash BLOB UNIQUE CHECK (
                            pending_credential_hash IS NULL
                            OR length(pending_credential_hash) = 32
                          ),
  credential_version      INTEGER NOT NULL DEFAULT 1 CHECK (credential_version > 0),
  revision                INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
  reported_model          TEXT CHECK (
                            reported_model IS NULL OR length(reported_model) <= 256
                          ),
  failure_code            TEXT CHECK (failure_code IS NULL OR failure_code IN (
                            'transport', 'protocol', 'credential_missing', 'credential_store'
                          )),
  created_at              INTEGER NOT NULL,
  updated_at              INTEGER NOT NULL,
  first_connected_at      INTEGER,
  last_seen_at            INTEGER,
  lease_expires_at        INTEGER,
  credential_expires_at   INTEGER,
  revoked_at              INTEGER,
  CHECK (can_create = 0 OR document_scope = 'all'),
  CHECK (
    (status = 'revoked' AND revoked_at IS NOT NULL)
    OR (status != 'revoked' AND revoked_at IS NULL)
  )
);

CREATE INDEX IF NOT EXISTS reviewer_connections_status
  ON reviewer_connections(status, updated_at DESC);
CREATE INDEX IF NOT EXISTS reviewer_connections_lease
  ON reviewer_connections(lease_expires_at)
  WHERE status = 'connected';

CREATE TABLE IF NOT EXISTS reviewer_connection_documents (
  connection_id TEXT    NOT NULL REFERENCES reviewer_connections(id),
  doc_id        TEXT    NOT NULL REFERENCES documents(id),
  created_at    INTEGER NOT NULL,
  PRIMARY KEY (connection_id, doc_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS reviewer_connection_documents_doc
  ON reviewer_connection_documents(doc_id, connection_id);

CREATE TABLE IF NOT EXISTS reviewer_connection_events (
  seq             INTEGER PRIMARY KEY AUTOINCREMENT,
  connection_id   TEXT    NOT NULL REFERENCES reviewer_connections(id),
  revision        INTEGER NOT NULL CHECK (revision > 0),
  event_type      TEXT    NOT NULL CHECK (event_type IN (
                    'created', 'renamed', 'permissions_changed', 'credential_rotated',
                    'connected', 'disconnected', 'failed', 'revoked'
                  )),
  display_label   TEXT    NOT NULL,
  status          TEXT    NOT NULL CHECK (status IN (
                    'configured', 'connected', 'disconnected', 'failed', 'revoked'
                  )),
  document_scope  TEXT    NOT NULL CHECK (document_scope IN ('selected', 'all')),
  can_read        INTEGER NOT NULL CHECK (can_read = 1),
  can_edit        INTEGER NOT NULL CHECK (can_edit IN (0, 1)),
  can_create      INTEGER NOT NULL CHECK (can_create IN (0, 1)),
  can_trash       INTEGER NOT NULL CHECK (can_trash IN (0, 1)),
  document_ids_json TEXT NOT NULL CHECK (
                      json_valid(document_ids_json)
                      AND json_type(document_ids_json) = 'array'
                      AND (
                        document_scope != 'all'
                        OR json_array_length(document_ids_json) = 0
                      )
                    ),
  failure_code    TEXT,
  created_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS reviewer_connection_events_connection
  ON reviewer_connection_events(connection_id, seq);

CREATE TRIGGER IF NOT EXISTS reviewer_connections_reject_delete
BEFORE DELETE ON reviewer_connections
BEGIN
  SELECT RAISE(ABORT, 'reviewer connections are never deleted');
END;

CREATE TRIGGER IF NOT EXISTS reviewer_connections_reject_identity_update
BEFORE UPDATE OF id, client, provider, created_at ON reviewer_connections
BEGIN
  SELECT RAISE(ABORT, 'reviewer connection identity is immutable');
END;

CREATE TRIGGER IF NOT EXISTS reviewer_connections_reject_revoked_update
BEFORE UPDATE ON reviewer_connections
WHEN OLD.revoked_at IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'revoked reviewer connections are immutable');
END;

CREATE TRIGGER IF NOT EXISTS reviewer_connections_reject_credential_collision_insert
BEFORE INSERT ON reviewer_connections
WHEN EXISTS (
  SELECT 1 FROM reviewer_connections
  WHERE pending_credential_hash = NEW.credential_hash
)
BEGIN
  SELECT RAISE(ABORT, 'reviewer credential hash already exists');
END;

CREATE TRIGGER IF NOT EXISTS reviewer_connections_reject_credential_collision_update
BEFORE UPDATE OF credential_hash, pending_credential_hash ON reviewer_connections
WHEN (
  NEW.pending_credential_hash IS NOT NULL
  AND NEW.pending_credential_hash = NEW.credential_hash
) OR EXISTS (
  SELECT 1 FROM reviewer_connections AS other
  WHERE other.id != NEW.id
    AND (
      other.credential_hash = NEW.credential_hash
      OR other.pending_credential_hash = NEW.credential_hash
      OR (
        NEW.pending_credential_hash IS NOT NULL
        AND (
          other.credential_hash = NEW.pending_credential_hash
          OR other.pending_credential_hash = NEW.pending_credential_hash
        )
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'reviewer credential hash already exists');
END;

CREATE TRIGGER IF NOT EXISTS reviewer_connection_events_reject_update
BEFORE UPDATE ON reviewer_connection_events
BEGIN
  SELECT RAISE(ABORT, 'reviewer connection events are append-only');
END;
CREATE TRIGGER IF NOT EXISTS reviewer_connection_events_reject_delete
BEFORE DELETE ON reviewer_connection_events
BEGIN
  SELECT RAISE(ABORT, 'reviewer connection events are append-only');
END;
"#;
