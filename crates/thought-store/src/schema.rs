//! Ordered SQLite migrations.
//!
//! Version 1 is intentionally the exact idempotent DDL used before migrations
//! existed. Both a fresh database and every released database therefore begin
//! at SQLite's default `user_version = 0`; applying V1 either creates the
//! baseline or adopts it without rewriting user data.

pub const CURRENT_VERSION: i64 = 4;

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
    Migration {
        version: 3,
        name: "anchored provenance evidence",
        sql: V3_PROVENANCE_ANCHORS,
    },
    Migration {
        version: 4,
        name: "durable reviewer connections",
        sql: V4_REVIEWER_CONNECTIONS,
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

/// Chain V2 adds exact grapheme-range evidence to the immutable ledger.
///
/// SQLite cannot alter a CHECK constraint in place. Rebuilding the parent also
/// requires rebuilding every table that references it because foreign keys are
/// enabled during migrations. All columns are copied explicitly and the
/// replacement tables retain the released constraints, composite foreign keys,
/// indexes, triggers, and AUTOINCREMENT keys.
const V3_PROVENANCE_ANCHORS: &str = r#"
CREATE TABLE provenance_events_v3 (
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
  actor_label          TEXT    NOT NULL,
  source_label         TEXT    NOT NULL CHECK (length(trim(source_label)) > 0),
  provider             TEXT,
  requested_model      TEXT,
  reported_model       TEXT,
  evidence_ref         TEXT,
  suggestion_id        TEXT,
  client_event_id      TEXT,
  chain_version        INTEGER NOT NULL DEFAULT 1 CHECK (chain_version IN (1, 2)),
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

INSERT INTO provenance_events_v3 (
  event_id, doc_id, update_seq, actor_id, action, ingress, assurance,
  connection_id, session_id, actor_label, source_label, provider,
  requested_model, reported_model, evidence_ref, suggestion_id,
  client_event_id, chain_version, before_hash, after_hash, update_log_root,
  previous_event_hash, event_hash, created_at, recorded_at
)
SELECT
  event_id, doc_id, update_seq, actor_id, action, ingress, assurance,
  connection_id, session_id, actor_label, source_label, provider,
  requested_model, reported_model, evidence_ref, suggestion_id,
  client_event_id, chain_version, before_hash, after_hash, update_log_root,
  previous_event_hash, event_hash, created_at, recorded_at
FROM provenance_events;

CREATE TABLE provenance_changes_v3 (
  event_id          INTEGER NOT NULL REFERENCES provenance_events_v3(event_id),
  ordinal           INTEGER NOT NULL CHECK (ordinal >= 0),
  op                TEXT    NOT NULL CHECK (op IN (
                        'insert', 'delete', 'format', 'structure'
                      )),
  source_event_id   INTEGER REFERENCES provenance_events_v3(event_id),
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

INSERT INTO provenance_changes_v3 (
  event_id, ordinal, op, source_event_id,
  before_block_id, before_path, before_from_utf16, before_to_utf16,
  after_block_id, after_path, after_from_utf16, after_to_utf16,
  before_text, after_text, before_format, after_format,
  before_shape, after_shape
)
SELECT
  event_id, ordinal, op, source_event_id,
  before_block_id, before_path, before_from_utf16, before_to_utf16,
  after_block_id, after_path, after_from_utf16, after_to_utf16,
  before_text, after_text, before_format, after_format,
  before_shape, after_shape
FROM provenance_changes;

CREATE TABLE provenance_receipts_v3 (
  seq             INTEGER PRIMARY KEY AUTOINCREMENT,
  receipt_id      TEXT    NOT NULL UNIQUE,
  event_id        INTEGER NOT NULL REFERENCES provenance_events_v3(event_id),
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

INSERT INTO provenance_receipts_v3 (
  seq, receipt_id, event_id, receipt_kind, issuer,
  artifact_digest, payload_format, payload, created_at
)
SELECT
  seq, receipt_id, event_id, receipt_kind, issuer,
  artifact_digest, payload_format, payload, created_at
FROM provenance_receipts;

CREATE TABLE lineage_spans_v3 (
  doc_id          TEXT    NOT NULL REFERENCES documents(id),
  block_id        TEXT    NOT NULL,
  node_path       TEXT    NOT NULL,
  start_utf16     INTEGER NOT NULL CHECK (start_utf16 >= 0),
  end_utf16       INTEGER NOT NULL CHECK (end_utf16 > start_utf16),
  source_event_id INTEGER NOT NULL,
  PRIMARY KEY (doc_id, block_id, node_path, start_utf16),
  FOREIGN KEY (doc_id, source_event_id)
    REFERENCES provenance_events_v3(doc_id, event_id)
) WITHOUT ROWID;

INSERT INTO lineage_spans_v3 (
  doc_id, block_id, node_path, start_utf16, end_utf16, source_event_id
)
SELECT doc_id, block_id, node_path, start_utf16, end_utf16, source_event_id
FROM lineage_spans;

-- Parent-table DROP runs an implicit DELETE, so remove its append-only trigger
-- only inside this migration transaction. The child ledgers must be dropped
-- first while foreign-key enforcement remains enabled.
DROP TRIGGER provenance_events_reject_update;
DROP TRIGGER provenance_events_reject_delete;
DROP TRIGGER provenance_changes_reject_update;
DROP TRIGGER provenance_changes_reject_delete;
DROP TRIGGER provenance_receipts_reject_update;
DROP TRIGGER provenance_receipts_reject_delete;

DROP TABLE provenance_changes;
DROP TABLE provenance_receipts;
DROP TABLE lineage_spans;
DROP TABLE provenance_events;

ALTER TABLE provenance_events_v3 RENAME TO provenance_events;
ALTER TABLE provenance_changes_v3 RENAME TO provenance_changes;
ALTER TABLE provenance_receipts_v3 RENAME TO provenance_receipts;
ALTER TABLE lineage_spans_v3 RENAME TO lineage_spans;

CREATE INDEX provenance_events_doc_event
  ON provenance_events(doc_id, event_id);
CREATE INDEX provenance_events_actor
  ON provenance_events(doc_id, actor_id, event_id);
CREATE INDEX provenance_receipts_event
  ON provenance_receipts(event_id, seq);
CREATE INDEX lineage_spans_source
  ON lineage_spans(source_event_id);

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

-- Each row anchors one exact operation range in both the pre- and
-- post-transaction grapheme coordinate spaces. Vector order is persisted as
-- the event-local ordinal.
CREATE TABLE provenance_anchors (
  event_id               INTEGER NOT NULL REFERENCES provenance_events(event_id),
  ordinal                INTEGER NOT NULL CHECK (ordinal >= 0),
  basis                  TEXT    NOT NULL CHECK (basis IN (
                           'editor_transaction', 'server_operation'
                         )),
  before_start_grapheme  INTEGER NOT NULL CHECK (before_start_grapheme >= 0),
  before_end_grapheme    INTEGER NOT NULL CHECK (before_end_grapheme >= before_start_grapheme),
  after_start_grapheme   INTEGER NOT NULL CHECK (after_start_grapheme >= 0),
  after_end_grapheme     INTEGER NOT NULL CHECK (after_end_grapheme >= after_start_grapheme),
  before_text_hash       BLOB    NOT NULL CHECK (length(before_text_hash) = 32),
  after_text_hash        BLOB    NOT NULL CHECK (length(after_text_hash) = 32),
  PRIMARY KEY (event_id, ordinal)
);

CREATE TRIGGER provenance_anchors_reject_out_of_order_insert
BEFORE INSERT ON provenance_anchors
WHEN NEW.ordinal != COALESCE(
  (SELECT MAX(ordinal) + 1 FROM provenance_anchors WHERE event_id = NEW.event_id),
  0
)
BEGIN
  SELECT RAISE(ABORT, 'provenance anchor ordinal is out of order');
END;
CREATE TRIGGER provenance_anchors_reject_update
BEFORE UPDATE ON provenance_anchors
BEGIN
  SELECT RAISE(ABORT, 'provenance_anchors is append-only');
END;
CREATE TRIGGER provenance_anchors_reject_delete
BEFORE DELETE ON provenance_anchors
BEGIN
  SELECT RAISE(ABORT, 'provenance_anchors is append-only');
END;
"#;

/// Mutable reviewer authorization and append-only lifecycle snapshots.
///
/// A connection is never deleted or reused. Its current permissions and lease
/// are operational state, while every meaningful transition is copied into an
/// append-only event without credential material. Provenance events continue
/// to freeze the label and connection ID that applied to each document change.
const V4_REVIEWER_CONNECTIONS: &str = r#"
CREATE TABLE reviewer_connections (
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

CREATE INDEX reviewer_connections_status
  ON reviewer_connections(status, updated_at DESC);
CREATE INDEX reviewer_connections_lease
  ON reviewer_connections(lease_expires_at)
  WHERE status = 'connected';

CREATE TABLE reviewer_connection_documents (
  connection_id TEXT    NOT NULL REFERENCES reviewer_connections(id),
  doc_id        TEXT    NOT NULL REFERENCES documents(id),
  created_at    INTEGER NOT NULL,
  PRIMARY KEY (connection_id, doc_id)
) WITHOUT ROWID;
CREATE INDEX reviewer_connection_documents_doc
  ON reviewer_connection_documents(doc_id, connection_id);

CREATE TABLE reviewer_connection_events (
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
CREATE INDEX reviewer_connection_events_connection
  ON reviewer_connection_events(connection_id, seq);

CREATE TRIGGER reviewer_connections_reject_delete
BEFORE DELETE ON reviewer_connections
BEGIN
  SELECT RAISE(ABORT, 'reviewer connections are never deleted');
END;

CREATE TRIGGER reviewer_connections_reject_identity_update
BEFORE UPDATE OF id, client, provider, created_at ON reviewer_connections
BEGIN
  SELECT RAISE(ABORT, 'reviewer connection identity is immutable');
END;

CREATE TRIGGER reviewer_connections_reject_revoked_update
BEFORE UPDATE ON reviewer_connections
WHEN OLD.revoked_at IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'revoked reviewer connections are immutable');
END;

CREATE TRIGGER reviewer_connections_reject_credential_collision_insert
BEFORE INSERT ON reviewer_connections
WHEN EXISTS (
  SELECT 1 FROM reviewer_connections
  WHERE pending_credential_hash = NEW.credential_hash
)
BEGIN
  SELECT RAISE(ABORT, 'reviewer credential hash already exists');
END;

CREATE TRIGGER reviewer_connections_reject_credential_collision_update
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

CREATE TRIGGER reviewer_connection_events_reject_update
BEFORE UPDATE ON reviewer_connection_events
BEGIN
  SELECT RAISE(ABORT, 'reviewer connection events are append-only');
END;
CREATE TRIGGER reviewer_connection_events_reject_delete
BEFORE DELETE ON reviewer_connection_events
BEGIN
  SELECT RAISE(ABORT, 'reviewer connection events are append-only');
END;
"#;

#[cfg(test)]
mod tests {
    use super::{CURRENT_VERSION, V1_BASELINE, V2_PROVENANCE};
    use crate::{Store, StoreError};
    use rusqlite::{Connection, types::Value};
    use std::path::Path;

    fn create_v2_database(path: &Path) {
        let connection = Connection::open(path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection.execute_batch(V1_BASELINE).unwrap();
        connection.execute_batch(V2_PROVENANCE).unwrap();
        connection
            .execute_batch(
                r#"
                INSERT INTO actors (id, kind, display_name, model, color, first_seen)
                VALUES ('human:v2', 'human', 'V2 Writer', NULL, '#123456', 1);
                INSERT INTO documents (id, title, created_at, updated_at)
                VALUES ('doc', 'V2 draft', 2, 3);
                INSERT INTO updates (
                  seq, doc_id, payload, actor_id, origin, session_id, created_at, synced_at
                ) VALUES (4, 'doc', X'00FF10', 'human:v2', 'human', 'session', 3, NULL);
                INSERT INTO provenance_events (
                  event_id, doc_id, update_seq, actor_id, action, ingress, assurance,
                  connection_id, session_id, actor_label, source_label, provider,
                  requested_model, reported_model, evidence_ref, suggestion_id,
                  client_event_id, chain_version, before_hash, after_hash, update_log_root,
                  previous_event_hash, event_hash, created_at, recorded_at
                ) VALUES (
                  7, 'doc', 4, 'human:v2', 'edit', 'entered', 'observed',
                  NULL, 'session', 'V2 Writer', 'Written here', NULL,
                  NULL, NULL, NULL, NULL, 'v2-event', 1,
                  X'000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F',
                  X'1F1E1D1C1B1A191817161514131211100F0E0D0C0B0A09080706050403020100',
                  X'1010101010101010101010101010101010101010101010101010101010101010',
                  NULL,
                  X'7777777777777777777777777777777777777777777777777777777777777777',
                  4, 5
                );
                INSERT INTO provenance_changes (
                  event_id, ordinal, op, source_event_id,
                  before_block_id, before_path, before_from_utf16, before_to_utf16,
                  after_block_id, after_path, after_from_utf16, after_to_utf16,
                  before_text, after_text, before_format, after_format,
                  before_shape, after_shape
                ) VALUES (
                  7, 0, 'insert', 7, NULL, NULL, NULL, NULL,
                  'block', '[0]', 0, 2, '', 'v2', NULL, '{}', NULL, NULL
                );
                INSERT INTO provenance_receipts (
                  seq, receipt_id, event_id, receipt_kind, issuer,
                  artifact_digest, payload_format, payload, created_at
                ) VALUES (
                  9, 'receipt-v2', 7, 'device_signature', 'device',
                  X'ABCD', 'application/octet-stream', X'00FF80', 6
                );
                INSERT INTO lineage_spans (
                  doc_id, block_id, node_path, start_utf16, end_utf16, source_event_id
                ) VALUES ('doc', 'block', '[0]', 0, 2, 7);
                INSERT INTO lineage_state (
                  doc_id, algorithm_version, through_update_seq, through_event_id,
                  state, lineage_digest, rebuilt_at
                ) VALUES ('doc', 1, 4, 7, 'ready', X'00FF', 8);
                PRAGMA user_version = 2;
                "#,
            )
            .unwrap();
    }

    fn rows(connection: &Connection, sql: &str) -> Vec<Vec<Value>> {
        let mut statement = connection.prepare(sql).unwrap();
        let column_count = statement.column_count();
        statement
            .query_map([], |row| {
                (0..column_count)
                    .map(|column| row.get(column))
                    .collect::<rusqlite::Result<Vec<Value>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn evidence_rows(connection: &Connection) -> Vec<Vec<Vec<Value>>> {
        [
            "SELECT * FROM provenance_events ORDER BY event_id",
            "SELECT * FROM provenance_changes ORDER BY event_id, ordinal",
            "SELECT * FROM provenance_receipts ORDER BY seq",
            "SELECT * FROM lineage_spans ORDER BY doc_id, block_id, node_path, start_utf16",
            "SELECT * FROM lineage_state ORDER BY doc_id",
        ]
        .iter()
        .map(|sql| rows(connection, sql))
        .collect()
    }

    #[test]
    fn v2_upgrade_preserves_every_evidence_value_and_autoincrement_key() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("thought.db");
        create_v2_database(&path);
        let before = {
            let connection = Connection::open(&path).unwrap();
            evidence_rows(&connection)
        };

        drop(Store::open(&path).unwrap());

        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        assert_eq!(evidence_rows(&connection), before);
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            CURRENT_VERSION
        );
        assert_eq!(
            rows(
                &connection,
                "SELECT name, seq FROM sqlite_sequence
                 WHERE name IN ('provenance_events', 'provenance_receipts') ORDER BY name",
            ),
            vec![
                vec![Value::Text("provenance_events".into()), Value::Integer(7)],
                vec![Value::Text("provenance_receipts".into()), Value::Integer(9)],
            ]
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        let lineage_parent: String = connection
            .query_row(
                "SELECT \"table\" FROM pragma_foreign_key_list('lineage_spans')
                 WHERE \"from\" = 'source_event_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lineage_parent, "provenance_events");
        for object in [
            "provenance_events_doc_event",
            "provenance_events_actor",
            "provenance_receipts_event",
            "lineage_spans_source",
            "provenance_events_reject_update",
            "provenance_events_reject_delete",
            "provenance_changes_reject_update",
            "provenance_changes_reject_delete",
            "provenance_receipts_reject_update",
            "provenance_receipts_reject_delete",
        ] {
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1",
                        [object],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                1,
                "missing rebuilt schema object {object}"
            );
        }

        connection
            .execute(
                "INSERT INTO provenance_events (
                   doc_id, actor_id, action, ingress, assurance, actor_label, source_label,
                   chain_version, before_hash, after_hash, update_log_root, event_hash,
                   created_at, recorded_at
                 ) VALUES (
                   'doc', 'human:v2', 'edit', 'entered', 'observed', 'V2 Writer', 'Written here',
                   1, zeroblob(32), zeroblob(32), zeroblob(32),
                   X'8888888888888888888888888888888888888888888888888888888888888888',
                   9, 9
                 )",
                [],
            )
            .unwrap();
        assert_eq!(connection.last_insert_rowid(), 8);
        connection
            .execute(
                "INSERT INTO provenance_receipts (
                   receipt_id, event_id, receipt_kind, issuer,
                   artifact_digest, payload_format, payload, created_at
                 ) VALUES (
                   'receipt-v3', 8, 'device_signature', 'device',
                   X'EF', 'application/octet-stream', X'01', 10
                 )",
                [],
            )
            .unwrap();
        assert_eq!(connection.last_insert_rowid(), 10);
    }

    #[test]
    fn failed_v3_upgrade_restores_the_complete_v2_schema_and_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("thought.db");
        create_v2_database(&path);
        let before = {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute("CREATE TABLE provenance_anchors (sentinel BLOB)", [])
                .unwrap();
            connection
                .execute("INSERT INTO provenance_anchors VALUES (X'00FF')", [])
                .unwrap();
            evidence_rows(&connection)
        };

        assert!(matches!(
            Store::open(&path),
            Err(StoreError::MigrationFailed { version: 3, .. })
        ));

        let connection = Connection::open(&path).unwrap();
        assert_eq!(evidence_rows(&connection), before);
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row("SELECT sentinel FROM provenance_anchors", [], |row| row
                    .get::<_, Vec<u8>>(0),)
                .unwrap(),
            vec![0, 255]
        );
        for temporary in [
            "provenance_events_v3",
            "provenance_changes_v3",
            "provenance_receipts_v3",
            "lineage_spans_v3",
        ] {
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1",
                        [temporary],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0
            );
        }
        assert!(
            connection
                .execute(
                    "UPDATE provenance_events SET actor_label = 'changed' WHERE event_id = 7",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO provenance_events (
                   doc_id, action, ingress, assurance, actor_label, source_label,
                   chain_version, before_hash, after_hash, update_log_root, event_hash,
                   created_at, recorded_at
                 ) VALUES (
                   'doc', 'edit', 'entered', 'observed', 'Writer', 'Written here',
                   2, zeroblob(32), zeroblob(32), zeroblob(32),
                   X'9999999999999999999999999999999999999999999999999999999999999999',
                   9, 9
                 )",
                    [],
                )
                .is_err()
        );
    }
}
