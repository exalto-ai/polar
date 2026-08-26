//! Ordered SQLite migrations.
//!
//! Version 1 is intentionally the exact idempotent DDL used before migrations
//! existed. Both a fresh database and every released database therefore begin
//! at SQLite's default `user_version = 0`; applying V1 either creates the
//! baseline or adopts it without rewriting user data.

pub const CURRENT_VERSION: i64 = 2;

pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "baseline",
        sql: V1_BASELINE,
    },
    Migration {
        version: 2,
        name: "surviving-span provenance",
        sql: V2_PROVENANCE,
    },
];

/// The schema shipped before `PRAGMA user_version` was introduced.
///
/// Keep the `IF NOT EXISTS` clauses here. They are what lets V1 safely adopt
/// an existing version-0 database as well as initialize an empty one.
const V1_BASELINE: &str = r#"
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

CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts USING fts5(
  doc_id UNINDEXED, title, body, tokenize='porter unicode61'
);
"#;

/// The durable event ledger and its discardable surviving-lineage read model.
///
/// Unlike V1, these statements deliberately omit `IF NOT EXISTS`. A migration
/// and its `user_version` advance commit together, so a name collision at V2 is
/// evidence of schema drift and must fail rather than bless an unknown table.
const V2_PROVENANCE: &str = r#"
-- A composite parent key prevents an event for one document from pointing at
-- an update belonging to another document.
CREATE UNIQUE INDEX updates_doc_seq_unique ON updates(doc_id, seq);

-- Immutable envelope for one observed provenance event. Events that do not
-- mutate the CRDT, such as rejecting a suggestion, have a NULL update_seq.
CREATE TABLE provenance_events (
  event_id             INTEGER PRIMARY KEY AUTOINCREMENT CHECK (event_id > 0),
  doc_id               TEXT    NOT NULL REFERENCES documents(id),
  update_seq           INTEGER UNIQUE,
  actor_id             TEXT    REFERENCES actors(id),
  action               TEXT    NOT NULL CHECK (action IN (
                           'edit', 'trash', 'restore', 'legacy_seed',
                           'suggestion', 'accept', 'reject'
                         )),
  ingress              TEXT    NOT NULL CHECK (ingress IN (
                           'entered',
                           'pasted',
                           'command',
                           'imported',
                           'mcp',
                           'api',
                           'suggestion',
                           'unknown',
                           'legacy_unknown'
                         )),
  assurance            TEXT    NOT NULL CHECK (assurance IN (
                           'observed', 'reported', 'verified', 'unknown'
                         )),
  connection_id        TEXT,
  session_id           TEXT,
  -- Snapshot mutable actor metadata so reconnects and model changes cannot
  -- rewrite labels on historical events.
  actor_label          TEXT    NOT NULL,
  -- Frozen consumer label for the source of this change. This remains
  -- distinct from actor_label because a provider/reviewer label can differ
  -- from the local process or account that submitted the mutation.
  source_label         TEXT    NOT NULL CHECK (length(trim(source_label)) > 0),
  provider             TEXT,
  requested_model      TEXT,
  reported_model       TEXT,
  evidence_ref         TEXT,
  suggestion_id        TEXT,
  -- Supplied by a local peer and unique within a document. It makes a resend
  -- after a lost acknowledgement idempotent without trusting a global id.
  client_event_id      TEXT,
  -- V1 is one frozen evidence and reconciliation suite. A later value requires
  -- a migration plus version-dispatched verification before the DDL admits it.
  chain_version        INTEGER NOT NULL DEFAULT 1 CHECK (chain_version = 1),
  before_hash          BLOB    NOT NULL CHECK (length(before_hash) = 32),
  after_hash           BLOB    NOT NULL CHECK (length(after_hash) = 32),
  update_log_root      BLOB    NOT NULL CHECK (length(update_log_root) = 32),
  previous_event_hash  BLOB    CHECK (
                           previous_event_hash IS NULL OR length(previous_event_hash) = 32
                         ),
  event_hash           BLOB    NOT NULL UNIQUE CHECK (length(event_hash) = 32),
  created_at           INTEGER NOT NULL,
  recorded_at          INTEGER NOT NULL,
  CHECK (
    (ingress IN ('entered', 'pasted', 'command', 'imported') AND assurance = 'observed')
    OR (ingress IN ('unknown', 'legacy_unknown') AND assurance = 'unknown')
    OR (ingress = 'mcp' AND assurance = 'reported')
    OR (ingress = 'api' AND assurance = 'verified')
    OR (ingress = 'suggestion' AND assurance IN ('reported', 'verified'))
  ),
  UNIQUE (doc_id, client_event_id),
  UNIQUE (doc_id, event_id),
  FOREIGN KEY (doc_id, update_seq) REFERENCES updates(doc_id, seq)
);
CREATE INDEX provenance_events_doc_event
  ON provenance_events(doc_id, event_id);
CREATE INDEX provenance_events_actor
  ON provenance_events(doc_id, actor_id, event_id);

-- Exact ordered changes represented by an event. Text-node paths are
-- canonical JSON arrays relative to the stable top-level block, and offsets
-- use UTF-16 code units to match ProseMirror positions.
CREATE TABLE provenance_changes (
  event_id          INTEGER NOT NULL REFERENCES provenance_events(event_id),
  ordinal           INTEGER NOT NULL CHECK (ordinal >= 0),
  op                TEXT    NOT NULL CHECK (op IN (
                        'insert', 'delete', 'format', 'structure'
                      )),
  -- Inserted text points at this event. Deleted and formatted text point at
  -- the earlier event that supplied the affected wording. Structure has no
  -- text source.
  source_event_id   INTEGER REFERENCES provenance_events(event_id),
  before_block_id   TEXT,
  before_path       TEXT,
  before_from_utf16 INTEGER CHECK (before_from_utf16 IS NULL OR before_from_utf16 >= 0),
  before_to_utf16   INTEGER CHECK (before_to_utf16 IS NULL OR before_to_utf16 >= 0),
  after_block_id    TEXT,
  after_path        TEXT,
  after_from_utf16  INTEGER CHECK (after_from_utf16 IS NULL OR after_from_utf16 >= 0),
  after_to_utf16    INTEGER CHECK (after_to_utf16 IS NULL OR after_to_utf16 >= 0),
  before_text       TEXT    NOT NULL DEFAULT '',
  after_text        TEXT    NOT NULL DEFAULT '',
  before_format     TEXT,
  after_format      TEXT,
  before_shape      TEXT,
  after_shape       TEXT,
  PRIMARY KEY (event_id, ordinal),
  CHECK (
    before_from_utf16 IS NULL OR before_to_utf16 IS NULL
    OR before_from_utf16 <= before_to_utf16
  ),
  CHECK (
    after_from_utf16 IS NULL OR after_to_utf16 IS NULL
    OR after_from_utf16 <= after_to_utf16
  )
);

-- Evidence can strengthen an event after it was recorded without mutating
-- that event. Examples are an MCP tool receipt, a provider trace, and a Seal
-- publication receipt.
CREATE TABLE provenance_receipts (
  seq             INTEGER PRIMARY KEY AUTOINCREMENT,
  receipt_id      TEXT    NOT NULL UNIQUE,
  event_id        INTEGER NOT NULL REFERENCES provenance_events(event_id),
  receipt_kind    TEXT    NOT NULL CHECK (receipt_kind IN (
                      'mcp_tool', 'provider_trace', 'seal_publication', 'device_signature'
                    )),
  issuer          TEXT    NOT NULL,
  artifact_digest BLOB    NOT NULL,
  payload_format  TEXT    NOT NULL,
  payload         BLOB    NOT NULL,
  created_at      INTEGER NOT NULL,
  UNIQUE (event_id, receipt_kind, artifact_digest)
);
CREATE INDEX provenance_receipts_event
  ON provenance_receipts(event_id, seq);

-- Current surviving text only. This is a mutable, discardable read model. A
-- span points back to the event that supplied its wording.
CREATE TABLE lineage_spans (
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
CREATE INDEX lineage_spans_source
  ON lineage_spans(source_event_id);

-- Presence of a ready row, not the number of spans, says a rebuild completed.
-- This matters for empty documents and for crash recovery. V1 can rebuild only
-- its frozen algorithm version. A future version needs a migration and
-- version-dispatched verifier/reconciler before this cache may be invalidated.
CREATE TABLE lineage_state (
  doc_id             TEXT PRIMARY KEY REFERENCES documents(id),
  algorithm_version  INTEGER NOT NULL CHECK (algorithm_version > 0),
  through_update_seq INTEGER NOT NULL DEFAULT 0 CHECK (through_update_seq >= 0),
  through_event_id   INTEGER NOT NULL DEFAULT 0 CHECK (through_event_id >= 0),
  state              TEXT    NOT NULL CHECK (state IN ('ready', 'stale')),
  lineage_digest     BLOB    NOT NULL,
  rebuilt_at         INTEGER NOT NULL
);

-- SQLite permissions are process-wide, so make append-only an invariant of
-- the database itself. Corrections are new events or receipts.
CREATE TRIGGER updates_reject_invalid_origin
BEFORE INSERT ON updates
WHEN NEW.origin NOT IN ('human', 'agent', 'remote')
BEGIN
  SELECT RAISE(ABORT, 'unknown update origin');
END;
CREATE TRIGGER updates_reject_evidence_update
BEFORE UPDATE OF seq, doc_id, payload, actor_id, origin, session_id, created_at ON updates
BEGIN
  SELECT RAISE(ABORT, 'update evidence is append-only');
END;
CREATE TRIGGER updates_reject_delete
BEFORE DELETE ON updates
BEGIN
  SELECT RAISE(ABORT, 'update evidence is append-only');
END;

CREATE TRIGGER provenance_events_reject_update
BEFORE UPDATE ON provenance_events
BEGIN
  SELECT RAISE(ABORT, 'provenance_events is append-only');
END;
CREATE TRIGGER provenance_events_reject_delete
BEFORE DELETE ON provenance_events
BEGIN
  SELECT RAISE(ABORT, 'provenance_events is append-only');
END;

CREATE TRIGGER provenance_changes_reject_update
BEFORE UPDATE ON provenance_changes
BEGIN
  SELECT RAISE(ABORT, 'provenance_changes is append-only');
END;
CREATE TRIGGER provenance_changes_reject_delete
BEFORE DELETE ON provenance_changes
BEGIN
  SELECT RAISE(ABORT, 'provenance_changes is append-only');
END;

CREATE TRIGGER provenance_receipts_reject_update
BEFORE UPDATE ON provenance_receipts
BEGIN
  SELECT RAISE(ABORT, 'provenance_receipts is append-only');
END;
CREATE TRIGGER provenance_receipts_reject_delete
BEFORE DELETE ON provenance_receipts
BEGIN
  SELECT RAISE(ABORT, 'provenance_receipts is append-only');
END;
"#;
