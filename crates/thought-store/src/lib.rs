//! Durable storage: an append-only op log, periodic snapshots, and the actors
//! that wrote them (M1.4).
//!
//! Deliberately ignorant of CRDT semantics — it stores opaque update frames.
//! Two retention policies live here and they are not the same policy:
//! snapshots exist for *load performance* and may be discarded freely, while
//! the op log exists for *provenance* and is never compacted away, because the
//! activity feed and per-run revert read it (AD-13).

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::HashSet;
use std::path::Path;

mod schema;

/// The newest schema this binary knows how to open.
pub const SCHEMA_VERSION: i64 = schema::CURRENT_VERSION;

/// Store failures need one non-SQL variant: opening a database from a newer
/// app must fail explicitly instead of letting older code misread its schema.
#[derive(Debug)]
pub enum StoreError {
    Database(rusqlite::Error),
    FutureSchemaVersion {
        found: i64,
        supported: i64,
    },
    InvalidMigrationPlan(String),
    InvalidEventId(i64),
    InvalidProvenanceAnchors {
        event_id: i64,
        reason: String,
    },
    EventIdExhausted,
    UpdateSequenceExhausted,
    InvalidStoredOrigin {
        seq: i64,
        value: String,
    },
    UnexpectedUpdateSequence {
        expected: i64,
        actual: i64,
    },
    IdempotencyConflict {
        doc_id: String,
        client_event_id: String,
    },
    LineageSourceMismatch {
        doc_id: String,
        source_event_id: i64,
    },
    ProvenanceChangeSourceMismatch {
        doc_id: String,
        event_id: i64,
        source_event_id: i64,
    },
    ProvenanceChangeSourceInFuture {
        event_id: i64,
        source_event_id: i64,
    },
    MigrationFailed {
        version: i64,
        name: &'static str,
        source: rusqlite::Error,
    },
    ForeignKeyViolation {
        table: String,
        row_id: Option<i64>,
        parent: String,
        constraint: i64,
    },
    ReviewerConnectionNotFound(String),
    ReviewerConnectionRevoked(String),
    ReviewerConnectionRevisionConflict {
        id: String,
        expected: i64,
        actual: i64,
    },
    InvalidReviewerConnectionTransition(String),
}

/// Kept as an alias for the public API used by `thought-mcp`.
pub type SqlError = StoreError;

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Database(error) => write!(f, "sqlite: {error}"),
            StoreError::FutureSchemaVersion { found, supported } => write!(
                f,
                "database schema version {found} is newer than this app supports ({supported})"
            ),
            StoreError::InvalidMigrationPlan(message) => {
                write!(f, "invalid database migration plan: {message}")
            }
            StoreError::InvalidEventId(event_id) => {
                write!(f, "provenance event id must be positive, found {event_id}")
            }
            StoreError::InvalidProvenanceAnchors { event_id, reason } => {
                write!(
                    f,
                    "invalid anchors for provenance event {event_id}: {reason}"
                )
            }
            StoreError::EventIdExhausted => write!(f, "provenance event ids are exhausted"),
            StoreError::UpdateSequenceExhausted => write!(f, "update sequence ids are exhausted"),
            StoreError::InvalidStoredOrigin { seq, value } => {
                write!(f, "update {seq} has unknown origin `{value}`")
            }
            StoreError::UnexpectedUpdateSequence { expected, actual } => write!(
                f,
                "predicted update sequence {expected}, but SQLite inserted {actual}"
            ),
            StoreError::IdempotencyConflict {
                doc_id,
                client_event_id,
            } => write!(
                f,
                "client event id `{client_event_id}` was reused with different provenance in document `{doc_id}`"
            ),
            StoreError::LineageSourceMismatch {
                doc_id,
                source_event_id,
            } => write!(
                f,
                "lineage source event {source_event_id} does not belong to document `{doc_id}`"
            ),
            StoreError::ProvenanceChangeSourceMismatch {
                doc_id,
                event_id,
                source_event_id,
            } => write!(
                f,
                "provenance change source event {source_event_id} for event {event_id} does not belong to document `{doc_id}`"
            ),
            StoreError::ProvenanceChangeSourceInFuture {
                event_id,
                source_event_id,
            } => write!(
                f,
                "provenance change source event {source_event_id} is later than containing event {event_id}"
            ),
            StoreError::MigrationFailed {
                version,
                name,
                source,
            } => write!(f, "database migration {version} ({name}) failed: {source}"),
            StoreError::ForeignKeyViolation {
                table,
                row_id,
                parent,
                constraint,
            } => write!(
                f,
                "foreign key violation in {table} row {row_id:?}, parent {parent}, constraint {constraint}"
            ),
            StoreError::ReviewerConnectionNotFound(id) => {
                write!(f, "reviewer connection `{id}` was not found")
            }
            StoreError::ReviewerConnectionRevoked(id) => {
                write!(f, "reviewer connection `{id}` has been revoked")
            }
            StoreError::ReviewerConnectionRevisionConflict {
                id,
                expected,
                actual,
            } => write!(
                f,
                "reviewer connection `{id}` changed from revision {expected} to {actual}"
            ),
            StoreError::InvalidReviewerConnectionTransition(message) => {
                write!(f, "invalid reviewer connection transition: {message}")
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Database(error) | StoreError::MigrationFailed { source: error, .. } => {
                Some(error)
            }
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        StoreError::Database(error)
    }
}

/// Who wrote an update. Kept out of the CRDT because Yjs cannot carry it
/// (AD-1), and retrofitting it later would leave all prior history anonymous
/// (AD-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Human,
    Agent,
    Remote,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Human => "human",
            Origin::Agent => "agent",
            Origin::Remote => "remote",
        }
    }

    fn parse(s: &str) -> Option<Origin> {
        match s {
            "human" => Some(Origin::Human),
            "agent" => Some(Origin::Agent),
            "remote" => Some(Origin::Remote),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Actor {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub model: Option<String>,
    pub color: String,
}

/// One entry in the op log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedUpdate {
    pub seq: i64,
    pub payload: Vec<u8>,
    pub actor_id: String,
    pub origin: Origin,
    pub session_id: Option<String>,
    pub created_at: i64,
}

/// Exact immutable update-log row used for evidence hashing. Unlike the
/// activity-facing [`LoggedUpdate`], its origin remains the raw stored text so
/// verification cannot normalize an altered value before hashing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceUpdate {
    pub seq: i64,
    pub payload: Vec<u8>,
    pub actor_id: String,
    pub origin: String,
    pub session_id: Option<String>,
    pub created_at: i64,
}

/// Who has worked on a document, from the op log. Yjs cannot carry this
/// (AD-1), which is why the log exists.
#[derive(Debug, Clone)]
pub struct ActorActivity {
    pub actor_id: String,
    pub kind: String,
    pub display_name: String,
    pub model: Option<String>,
    pub color: String,
    pub last_seen: i64,
    pub edits: i64,
}

/// Who wrote one block, joined to the actor that wrote it.
///
/// `created_by` and `touched_by` are separate because they answer different
/// questions: where the text came from, and who last had a hand in it.
#[derive(Debug, Clone)]
pub struct BlockAttribution {
    pub block_id: String,
    pub created_by: String,
    pub created_at: i64,
    pub touched_by: String,
    pub touched_at: i64,
    pub session_id: Option<String>,
    /// From the actor row for `touched_by` — the rail's colour and label.
    pub kind: String,
    pub display_name: String,
    pub model: Option<String>,
    pub color: String,
}

#[derive(Debug, Clone)]
pub struct DocumentRow {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    pub deleted: bool,
}

fn document_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DocumentRow> {
    Ok(DocumentRow {
        id: row.get(0)?,
        title: row.get(1)?,
        updated_at: row.get(2)?,
        deleted: row.get::<_, Option<i64>>(3)?.is_some(),
    })
}

/// Durable reviewer identity and its current authorization state.
///
/// Credential hashes are native-only authentication material. They are
/// exposed to the daemon crate, but never serialized into the editor API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerConnectionRow {
    pub id: String,
    pub client: String,
    pub provider: String,
    pub display_label: String,
    pub status: String,
    pub document_scope: String,
    pub can_read: bool,
    pub can_edit: bool,
    pub can_create: bool,
    pub can_trash: bool,
    pub credential_hash: Vec<u8>,
    pub pending_credential_hash: Option<Vec<u8>>,
    pub credential_version: i64,
    pub revision: i64,
    pub reported_model: Option<String>,
    pub failure_code: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub first_connected_at: Option<i64>,
    pub last_seen_at: Option<i64>,
    pub lease_expires_at: Option<i64>,
    pub credential_expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub document_ids: Vec<String>,
}

pub struct NewReviewerConnectionInput<'a> {
    pub id: &'a str,
    pub client: &'a str,
    pub provider: &'a str,
    pub display_label: &'a str,
    pub document_scope: &'a str,
    pub can_edit: bool,
    pub can_create: bool,
    pub can_trash: bool,
    pub credential_hash: &'a [u8],
    pub credential_expires_at: Option<i64>,
    pub document_ids: &'a [String],
    pub created_at: i64,
}

/// Complete desired management state for an optimistic connection update.
pub struct ReviewerConnectionUpdateInput<'a> {
    pub id: &'a str,
    pub expected_revision: i64,
    pub display_label: &'a str,
    pub document_scope: &'a str,
    pub can_edit: bool,
    pub can_create: bool,
    pub can_trash: bool,
    pub document_ids: &'a [String],
    pub updated_at: i64,
}

/// Everything that makes a newly created document visible and recoverable.
/// Stored in one SQLite transaction so callers never observe a document row
/// without its CRDT state, search entry, and initial attribution.
pub struct InitialDocument<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub payload: &'a [u8],
    pub actor_id: &'a str,
    pub origin: Origin,
    pub session_id: Option<&'a str>,
    pub markdown: &'a str,
    pub block_ids: &'a [String],
    pub attributed_at: i64,
}

/// One opaque CRDT update to persist alongside a semantic provenance event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceUpdateInput {
    /// Explicit so the caller can bind the canonical event hash to this update.
    /// Obtain it from [`Store::next_update_seq`].
    pub expected_seq: i64,
    pub payload: Vec<u8>,
    pub actor_id: String,
    pub origin: Origin,
    pub session_id: Option<String>,
    pub created_at: i64,
}

/// Immutable metadata for an event. Hashes are computed by the caller because
/// their canonical format belongs to the provenance protocol, not SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceEventInput {
    /// Explicit so the lineage engine can use this event as a source before
    /// persistence. Obtain it from [`Store::next_provenance_event_id`].
    pub event_id: i64,
    pub actor_id: Option<String>,
    pub action: String,
    pub ingress: String,
    pub assurance: String,
    pub connection_id: Option<String>,
    pub session_id: Option<String>,
    pub actor_label: String,
    pub source_label: String,
    pub provider: Option<String>,
    pub requested_model: Option<String>,
    pub reported_model: Option<String>,
    pub evidence_ref: Option<String>,
    pub suggestion_id: Option<String>,
    pub client_event_id: Option<String>,
    pub chain_version: i64,
    pub before_hash: Vec<u8>,
    pub after_hash: Vec<u8>,
    pub update_log_root: Vec<u8>,
    pub previous_event_hash: Option<Vec<u8>>,
    pub event_hash: Vec<u8>,
    pub created_at: i64,
    pub recorded_at: i64,
}

/// One ordered semantic delta row. Vector order determines `ordinal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceChangeInput {
    pub op: String,
    pub source_event_id: Option<i64>,
    pub before_block_id: Option<String>,
    pub before_path: Option<String>,
    pub before_from_utf16: Option<i64>,
    pub before_to_utf16: Option<i64>,
    pub after_block_id: Option<String>,
    pub after_path: Option<String>,
    pub after_from_utf16: Option<i64>,
    pub after_to_utf16: Option<i64>,
    pub before_text: String,
    pub after_text: String,
    pub before_format: Option<String>,
    pub after_format: Option<String>,
    pub before_shape: Option<String>,
    pub after_shape: Option<String>,
}

/// One exact operation range bound to both sides of a provenance event.
///
/// Offsets count Unicode grapheme clusters in the canonical visible-text
/// projection. The hashes bind the exact text inside the corresponding range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceAnchorInput {
    pub basis: String,
    pub before_start_grapheme: i64,
    pub before_end_grapheme: i64,
    pub after_start_grapheme: i64,
    pub after_end_grapheme: i64,
    pub before_text_hash: Vec<u8>,
    pub after_text_hash: Vec<u8>,
}

/// One current text range attributed to the event that supplied its wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageSpanInput {
    pub block_id: String,
    pub node_path: String,
    pub start_utf16: i64,
    pub end_utf16: i64,
    pub source_event_id: i64,
}

/// Metadata proving the accompanying live spans form one complete generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyLineageInput {
    pub algorithm_version: i64,
    pub lineage_digest: Vec<u8>,
    pub rebuilt_at: i64,
}

/// Compatibility update for the block-level rails while the new span UI is
/// introduced. First touch sets creation, later touches update only recency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockTouchInput {
    pub block_id: String,
    pub actor_id: String,
    pub session_id: Option<String>,
    pub at: i64,
}

/// Normal document mutation, persisted as one indivisible operation.
#[derive(Debug, Clone)]
pub struct ProvenanceCommitInput {
    pub doc_id: String,
    pub title: String,
    pub markdown: String,
    pub updated_at: i64,
    /// Current CRDT tombstone projected into the document list cache.
    pub deleted_at: Option<i64>,
    pub actor: Actor,
    pub update: ProvenanceUpdateInput,
    pub event: ProvenanceEventInput,
    pub changes: Vec<ProvenanceChangeInput>,
    pub anchors: Vec<ProvenanceAnchorInput>,
    pub spans: Vec<LineageSpanInput>,
    pub lineage: ReadyLineageInput,
    pub block_touches: Vec<BlockTouchInput>,
    /// Complete current top-level ID set. Compatibility rows absent from this
    /// list are removed with parameterized statements in the same transaction.
    pub current_block_ids: Vec<String>,
}

/// New document creation plus its first update and complete provenance state.
#[derive(Debug, Clone)]
pub struct InitialProvenanceDocumentInput {
    pub id: String,
    pub title: String,
    pub markdown: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub actor: Actor,
    pub update: ProvenanceUpdateInput,
    pub event: ProvenanceEventInput,
    pub changes: Vec<ProvenanceChangeInput>,
    pub anchors: Vec<ProvenanceAnchorInput>,
    pub spans: Vec<LineageSpanInput>,
    pub lineage: ReadyLineageInput,
    /// Compatibility rows for the existing M2 attribution rails.
    pub block_ids: Vec<String>,
    pub attributed_at: i64,
}

/// Provenance-only event, primarily the conservative seed for an old document.
#[derive(Debug, Clone)]
pub struct ProvenanceRecordInput {
    pub doc_id: String,
    pub event: ProvenanceEventInput,
    pub changes: Vec<ProvenanceChangeInput>,
    pub anchors: Vec<ProvenanceAnchorInput>,
    pub spans: Vec<LineageSpanInput>,
    pub lineage: ReadyLineageInput,
    /// Bind the event to the document's current final update without appending
    /// a new update. Legacy seeding uses this to record its exact baseline.
    pub bind_to_latest_update: bool,
}

/// Rebuild only the discardable lineage cache from immutable event history.
#[derive(Debug, Clone)]
pub struct LineageRebuildInput {
    pub doc_id: String,
    pub spans: Vec<LineageSpanInput>,
    pub lineage: ReadyLineageInput,
    pub through_update_seq: i64,
    pub through_event_id: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistedProvenance {
    pub update_seq: Option<i64>,
    pub event_id: i64,
    /// True when this exact client event was already durable and no rows were
    /// written by the call.
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceEventRow {
    pub event_id: i64,
    pub doc_id: String,
    pub update_seq: Option<i64>,
    pub actor_id: Option<String>,
    pub action: String,
    pub ingress: String,
    pub assurance: String,
    pub connection_id: Option<String>,
    pub session_id: Option<String>,
    pub actor_label: String,
    pub source_label: String,
    pub provider: Option<String>,
    pub requested_model: Option<String>,
    pub reported_model: Option<String>,
    pub evidence_ref: Option<String>,
    pub suggestion_id: Option<String>,
    pub client_event_id: Option<String>,
    pub chain_version: i64,
    pub before_hash: Vec<u8>,
    pub after_hash: Vec<u8>,
    pub update_log_root: Vec<u8>,
    pub previous_event_hash: Option<Vec<u8>>,
    pub event_hash: Vec<u8>,
    pub created_at: i64,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceChangeRow {
    pub event_id: i64,
    pub ordinal: i64,
    pub change: ProvenanceChangeInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceAnchorRow {
    pub event_id: i64,
    pub ordinal: i64,
    pub anchor: ProvenanceAnchorInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageSpanRow {
    pub doc_id: String,
    pub span: LineageSpanInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageStateRow {
    pub doc_id: String,
    pub algorithm_version: i64,
    pub through_update_seq: i64,
    pub through_event_id: i64,
    pub state: String,
    pub lineage_digest: Vec<u8>,
    pub rebuilt_at: i64,
}

/// What a cold start needs: the newest snapshot, plus every update after it.
#[derive(Debug, Default)]
pub struct Restored {
    pub snapshot: Option<Vec<u8>>,
    pub updates: Vec<Vec<u8>>,
    pub through_seq: i64,
}

pub struct Store {
    conn: Connection,
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    validate_migration_plan()?;

    let found: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if found > schema::CURRENT_VERSION {
        return Err(StoreError::FutureSchemaVersion {
            found,
            supported: schema::CURRENT_VERSION,
        });
    }

    let mut applied = false;
    for migration in schema::MIGRATIONS
        .iter()
        .filter(|migration| migration.version > found)
    {
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| StoreError::MigrationFailed {
                version: migration.version,
                name: migration.name,
                source,
            })?;
        transaction
            .execute_batch(migration.sql)
            .map_err(|source| StoreError::MigrationFailed {
                version: migration.version,
                name: migration.name,
                source,
            })?;
        transaction
            .pragma_update(None, "user_version", migration.version)
            .map_err(|source| StoreError::MigrationFailed {
                version: migration.version,
                name: migration.name,
                source,
            })?;
        validate_foreign_keys(&transaction)?;
        transaction
            .commit()
            .map_err(|source| StoreError::MigrationFailed {
                version: migration.version,
                name: migration.name,
                source,
            })?;
        applied = true;
    }

    // A current database still gets checked. `foreign_keys = ON` prevents new
    // violations, while this catches a copied or externally modified store.
    if !applied {
        validate_foreign_keys(conn)?;
    }
    Ok(())
}

fn validate_migration_plan() -> Result<(), StoreError> {
    for (index, migration) in schema::MIGRATIONS.iter().enumerate() {
        let expected = index as i64 + 1;
        if migration.version != expected {
            return Err(StoreError::InvalidMigrationPlan(format!(
                "expected version {expected}, found {} ({})",
                migration.version, migration.name
            )));
        }
        if migration.name.trim().is_empty() {
            return Err(StoreError::InvalidMigrationPlan(format!(
                "version {} has no name",
                migration.version
            )));
        }
    }
    if schema::MIGRATIONS.last().map(|migration| migration.version) != Some(schema::CURRENT_VERSION)
    {
        return Err(StoreError::InvalidMigrationPlan(format!(
            "current version {} does not match the final migration",
            schema::CURRENT_VERSION
        )));
    }
    Ok(())
}

fn validate_foreign_keys(conn: &Connection) -> Result<(), StoreError> {
    let mut statement = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = statement.query([])?;
    if let Some(row) = rows.next()? {
        return Err(StoreError::ForeignKeyViolation {
            table: row.get(0)?,
            row_id: row.get(1)?,
            parent: row.get(2)?,
            constraint: row.get(3)?,
        });
    }
    Ok(())
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Store, SqlError> {
        Store::wrap(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store, SqlError> {
        Store::wrap(Connection::open_in_memory()?)
    }

    fn wrap(mut conn: Connection) -> Result<Store, SqlError> {
        // Foreign keys are connection-local and must cover migrations too.
        // Check the schema before changing persistent journaling settings so
        // refusing a future database leaves it untouched.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn)?;
        // WAL so a reader never blocks the writer. NORMAL trades a fsync per
        // commit for the small risk of losing the last few updates on power
        // loss — acceptable because the relay and the peer replicas hold them
        // too, and unacceptable latency is worse for a writing app.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(Store { conn })
    }

    /// The next event id under AD-2's single-writer daemon assumption.
    ///
    /// This is not a database reservation. The caller needs the positive id to
    /// compute semantic lineage before persistence, then supplies it to the
    /// final transaction. The explicit event INSERT remains the collision
    /// check if the single-writer assumption is ever violated.
    pub fn next_provenance_event_id(&self) -> Result<i64, SqlError> {
        let current: Option<i64> =
            self.conn
                .query_row("SELECT MAX(event_id) FROM provenance_events", [], |row| {
                    row.get(0)
                })?;
        current
            .unwrap_or(0)
            .checked_add(1)
            .filter(|event_id| *event_id > 0)
            .ok_or(StoreError::EventIdExhausted)
    }

    /// The next update sequence under the same single-writer assumption as
    /// [`Store::next_provenance_event_id`].
    ///
    /// The canonical event hash binds this value before persistence. The final
    /// transaction inserts it explicitly, so a collision fails and rolls back,
    /// and the inserted row id is checked before anything commits.
    pub fn next_update_seq(&self) -> Result<i64, SqlError> {
        let current: i64 = self.conn.query_row(
            "SELECT COALESCE(
                 (SELECT seq FROM sqlite_sequence WHERE name = 'updates'), 0
             )",
            [],
            |row| row.get(0),
        )?;
        current
            .checked_add(1)
            .filter(|seq| *seq > 0)
            .ok_or(StoreError::UpdateSequenceExhausted)
    }

    /// Register one durable reviewer identity. The raw credential is stored by
    /// the native credential store, so SQLite receives only its fixed hash.
    pub fn create_reviewer_connection(
        &self,
        input: &NewReviewerConnectionInput<'_>,
    ) -> Result<ReviewerConnectionRow, SqlError> {
        validate_new_reviewer_connection(input)?;
        let document_ids = normalized_document_ids(input.document_scope, input.document_ids);
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO reviewer_connections (
                 id, client, provider, display_label, status, document_scope,
                 can_read, can_edit, can_create, can_trash, credential_hash,
                 credential_expires_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 'configured', ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
            params![
                input.id,
                input.client,
                input.provider,
                input.display_label.trim(),
                input.document_scope,
                input.can_edit,
                input.can_create,
                input.can_trash,
                input.credential_hash,
                input.credential_expires_at,
                input.created_at
            ],
        )?;
        replace_reviewer_documents(&transaction, input.id, &document_ids, input.created_at)?;
        insert_reviewer_connection_event(&transaction, input.id, "created", input.created_at)?;
        transaction.commit()?;
        self.reviewer_connection(input.id)
    }

    /// Current connection state, including its selected-document allowlist.
    pub fn reviewer_connection(&self, id: &str) -> Result<ReviewerConnectionRow, SqlError> {
        reviewer_connection_from_connection(&self.conn, id)
    }

    /// Non-secret generation used to reject a native failure report issued by
    /// a launcher that predates the most recent credential reset.
    pub fn reviewer_credential_version(&self, id: &str) -> Result<i64, SqlError> {
        self.conn
            .query_row(
                "SELECT credential_version FROM reviewer_connections WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::ReviewerConnectionNotFound(id.to_string()))
    }

    /// All saved connections. Expired live leases are projected to a durable
    /// disconnected state before the rows are returned.
    pub fn list_reviewer_connections(
        &self,
        at: i64,
    ) -> Result<Vec<ReviewerConnectionRow>, SqlError> {
        self.expire_reviewer_leases(at)?;
        let mut statement = self.conn.prepare(
            "SELECT id FROM reviewer_connections
             ORDER BY CASE status WHEN 'revoked' THEN 1 ELSE 0 END,
                      created_at DESC, id",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        ids.iter()
            .map(|id| reviewer_connection_from_connection(&self.conn, id))
            .collect()
    }

    /// Resolve either the current hash or an in-progress rotation hash. The
    /// latter keeps a crash between Keychain replacement and SQLite finalizing
    /// from locking out an otherwise valid connection.
    pub fn reviewer_connection_by_credential_hash(
        &self,
        credential_hash: &[u8],
        at: i64,
    ) -> Result<Option<ReviewerConnectionRow>, SqlError> {
        if credential_hash.len() != 32 {
            return Ok(None);
        }
        let id = self
            .conn
            .query_row(
                "SELECT id FROM reviewer_connections
                 WHERE status != 'revoked'
                   AND (credential_expires_at IS NULL OR credential_expires_at > ?2)
                   AND (credential_hash = ?1 OR pending_credential_hash = ?1)
                 LIMIT 1",
                params![credential_hash, at],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        id.map(|id| reviewer_connection_from_connection(&self.conn, &id))
            .transpose()
    }

    /// Native crash-recovery probe. `None` means there is no staged rotation;
    /// `Some(true)` means the supplied native secret matches the staged hash.
    pub fn reviewer_pending_credential_matches(
        &self,
        id: &str,
        credential_hash: &[u8],
    ) -> Result<Option<bool>, SqlError> {
        if credential_hash.len() != 32 {
            return Ok(Some(false));
        }
        let pending = self
            .conn
            .query_row(
                "SELECT pending_credential_hash FROM reviewer_connections WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::ReviewerConnectionNotFound(id.to_string()))?;
        Ok(pending.map(|pending| pending == credential_hash))
    }

    pub fn reviewer_has_pending_credential(&self, id: &str) -> Result<bool, SqlError> {
        self.conn
            .query_row(
                "SELECT pending_credential_hash IS NOT NULL
                 FROM reviewer_connections WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::ReviewerConnectionNotFound(id.to_string()))
    }

    /// Replace the user-managed label, permissions, and document scope using
    /// optimistic revision control. A no-op does not consume a revision.
    pub fn update_reviewer_connection(
        &self,
        input: &ReviewerConnectionUpdateInput<'_>,
    ) -> Result<ReviewerConnectionRow, SqlError> {
        validate_reviewer_management_state(
            input.display_label,
            input.document_scope,
            input.can_create,
            input.document_ids,
        )?;
        let desired_documents = normalized_document_ids(input.document_scope, input.document_ids);
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let before = reviewer_connection_from_connection(&transaction, input.id)?;
        ensure_reviewer_mutable(&before, input.expected_revision)?;

        let renamed = before.display_label != input.display_label.trim();
        let permissions_changed = before.document_scope != input.document_scope
            || before.can_edit != input.can_edit
            || before.can_create != input.can_create
            || before.can_trash != input.can_trash
            || before.document_ids != desired_documents;
        if !renamed && !permissions_changed {
            transaction.commit()?;
            return Ok(before);
        }

        let changed = transaction.execute(
            "UPDATE reviewer_connections
             SET display_label = ?3, document_scope = ?4,
                 can_edit = ?5, can_create = ?6, can_trash = ?7,
                 revision = revision + 1, updated_at = ?8
             WHERE id = ?1 AND revision = ?2 AND revoked_at IS NULL",
            params![
                input.id,
                input.expected_revision,
                input.display_label.trim(),
                input.document_scope,
                input.can_edit,
                input.can_create,
                input.can_trash,
                input.updated_at
            ],
        )?;
        if changed != 1 {
            return Err(reviewer_revision_error(
                &transaction,
                input.id,
                input.expected_revision,
            ));
        }
        replace_reviewer_documents(&transaction, input.id, &desired_documents, input.updated_at)?;
        if renamed {
            insert_reviewer_connection_event(&transaction, input.id, "renamed", input.updated_at)?;
        }
        if permissions_changed {
            insert_reviewer_connection_event(
                &transaction,
                input.id,
                "permissions_changed",
                input.updated_at,
            )?;
        }
        transaction.commit()?;
        self.reviewer_connection(input.id)
    }

    /// Stage a credential hash before replacing the native secret. No public
    /// revision changes until the native write has succeeded and rotation is
    /// finalized.
    pub fn begin_reviewer_credential_rotation(
        &self,
        id: &str,
        expected_revision: i64,
        pending_credential_hash: &[u8],
        updated_at: i64,
    ) -> Result<(), SqlError> {
        if pending_credential_hash.len() != 32 {
            return Err(StoreError::InvalidReviewerConnectionTransition(
                "credential hashes must contain 32 bytes".to_string(),
            ));
        }
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let before = reviewer_connection_from_connection(&transaction, id)?;
        ensure_reviewer_mutable(&before, expected_revision)?;
        if before.pending_credential_hash.is_some() {
            return Err(StoreError::InvalidReviewerConnectionTransition(format!(
                "reviewer connection `{id}` already has a pending credential rotation"
            )));
        }
        transaction.execute(
            "UPDATE reviewer_connections
             SET pending_credential_hash = ?3, updated_at = ?4
             WHERE id = ?1 AND revision = ?2 AND revoked_at IS NULL",
            params![id, expected_revision, pending_credential_hash, updated_at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn finish_reviewer_credential_rotation(
        &self,
        id: &str,
        expected_revision: i64,
        updated_at: i64,
    ) -> Result<ReviewerConnectionRow, SqlError> {
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let before = reviewer_connection_from_connection(&transaction, id)?;
        ensure_reviewer_mutable(&before, expected_revision)?;
        if before.pending_credential_hash.is_none() {
            return Err(StoreError::InvalidReviewerConnectionTransition(format!(
                "reviewer connection `{id}` has no pending credential rotation"
            )));
        }
        transaction.execute(
            "UPDATE reviewer_connections
             SET credential_hash = pending_credential_hash,
                 pending_credential_hash = NULL,
                 credential_version = credential_version + 1,
                 revision = revision + 1,
                 status = 'disconnected',
                 lease_expires_at = NULL,
                 updated_at = ?3,
                 failure_code = NULL
             WHERE id = ?1 AND revision = ?2 AND revoked_at IS NULL",
            params![id, expected_revision, updated_at],
        )?;
        insert_reviewer_connection_event(&transaction, id, "credential_rotated", updated_at)?;
        if before.status != "disconnected" {
            insert_reviewer_connection_event(&transaction, id, "disconnected", updated_at)?;
        }
        transaction.commit()?;
        self.reviewer_connection(id)
    }

    pub fn cancel_reviewer_credential_rotation(
        &self,
        id: &str,
        expected_revision: i64,
        updated_at: i64,
    ) -> Result<(), SqlError> {
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let before = reviewer_connection_from_connection(&transaction, id)?;
        ensure_reviewer_mutable(&before, expected_revision)?;
        transaction.execute(
            "UPDATE reviewer_connections
             SET pending_credential_hash = NULL, updated_at = ?3
             WHERE id = ?1 AND revision = ?2 AND revoked_at IS NULL",
            params![id, expected_revision, updated_at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Permanently remove a reviewer's authority while retaining its identity
    /// and lifecycle audit record.
    pub fn revoke_reviewer_connection(
        &self,
        id: &str,
        expected_revision: i64,
        revoked_at: i64,
    ) -> Result<ReviewerConnectionRow, SqlError> {
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let before = reviewer_connection_from_connection(&transaction, id)?;
        ensure_reviewer_mutable(&before, expected_revision)?;
        let changed = transaction.execute(
            "UPDATE reviewer_connections
             SET status = 'revoked', revoked_at = ?3, updated_at = ?3,
                 lease_expires_at = NULL, pending_credential_hash = NULL,
                 failure_code = NULL, revision = revision + 1
             WHERE id = ?1 AND revision = ?2 AND revoked_at IS NULL",
            params![id, expected_revision, revoked_at],
        )?;
        if changed != 1 {
            return Err(reviewer_revision_error(&transaction, id, expected_revision));
        }
        insert_reviewer_connection_event(&transaction, id, "revoked", revoked_at)?;
        transaction.commit()?;
        self.reviewer_connection(id)
    }

    /// Mark authenticated traffic and extend the connection lease. Reported
    /// model text remains self-reported and is bounded by the schema.
    pub fn mark_reviewer_seen(
        &self,
        id: &str,
        seen_at: i64,
        lease_expires_at: i64,
        reported_model: Option<&str>,
    ) -> Result<ReviewerConnectionRow, SqlError> {
        if lease_expires_at <= seen_at {
            return Err(StoreError::InvalidReviewerConnectionTransition(
                "a reviewer lease must expire after its heartbeat".to_string(),
            ));
        }
        if reported_model.is_some_and(|model| model.len() > 256) {
            return Err(StoreError::InvalidReviewerConnectionTransition(
                "reported model labels are limited to 256 bytes".to_string(),
            ));
        }
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let before = reviewer_connection_from_connection(&transaction, id)?;
        if before.revoked_at.is_some() {
            return Err(StoreError::ReviewerConnectionRevoked(id.to_string()));
        }
        transaction.execute(
            "UPDATE reviewer_connections
             SET status = 'connected', first_connected_at = COALESCE(first_connected_at, ?2),
                 last_seen_at = ?2, lease_expires_at = ?3, updated_at = ?2,
                 reported_model = COALESCE(?4, reported_model), failure_code = NULL
             WHERE id = ?1 AND revoked_at IS NULL",
            params![id, seen_at, lease_expires_at, reported_model],
        )?;
        if before.status != "connected" {
            insert_reviewer_connection_event(&transaction, id, "connected", seen_at)?;
        }
        transaction.commit()?;
        self.reviewer_connection(id)
    }

    pub fn mark_reviewer_disconnected(
        &self,
        id: &str,
        disconnected_at: i64,
    ) -> Result<ReviewerConnectionRow, SqlError> {
        self.set_reviewer_operational_status(id, "disconnected", None, disconnected_at)
    }

    pub fn mark_reviewer_failed(
        &self,
        id: &str,
        failure_code: &str,
        failed_at: i64,
    ) -> Result<ReviewerConnectionRow, SqlError> {
        if !matches!(
            failure_code,
            "transport" | "protocol" | "credential_missing" | "credential_store"
        ) {
            return Err(StoreError::InvalidReviewerConnectionTransition(format!(
                "unknown reviewer failure code `{failure_code}`"
            )));
        }
        self.set_reviewer_operational_status(id, "failed", Some(failure_code), failed_at)
    }

    /// Convert expired live leases into durable disconnected lifecycle state.
    pub fn expire_reviewer_leases(&self, at: i64) -> Result<usize, SqlError> {
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT id FROM reviewer_connections
             WHERE status = 'connected' AND lease_expires_at IS NOT NULL
               AND lease_expires_at <= ?1
             ORDER BY id",
        )?;
        let ids = statement
            .query_map(params![at], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for id in &ids {
            transaction.execute(
                "UPDATE reviewer_connections
                 SET status = 'disconnected', lease_expires_at = NULL,
                     updated_at = ?2, failure_code = NULL
                 WHERE id = ?1 AND status = 'connected'",
                params![id, at],
            )?;
            insert_reviewer_connection_event(&transaction, id, "disconnected", at)?;
        }
        transaction.commit()?;
        Ok(ids.len())
    }

    pub fn update_reviewer_reported_model(
        &self,
        id: &str,
        reported_model: Option<&str>,
        updated_at: i64,
    ) -> Result<ReviewerConnectionRow, SqlError> {
        if reported_model.is_some_and(|model| model.len() > 256) {
            return Err(StoreError::InvalidReviewerConnectionTransition(
                "reported model labels are limited to 256 bytes".to_string(),
            ));
        }
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let before = reviewer_connection_from_connection(&transaction, id)?;
        if before.revoked_at.is_some() {
            return Err(StoreError::ReviewerConnectionRevoked(id.to_string()));
        }
        transaction.execute(
            "UPDATE reviewer_connections
             SET reported_model = ?2, updated_at = ?3
             WHERE id = ?1 AND revoked_at IS NULL",
            params![id, reported_model, updated_at],
        )?;
        transaction.commit()?;
        self.reviewer_connection(id)
    }

    fn set_reviewer_operational_status(
        &self,
        id: &str,
        status: &str,
        failure_code: Option<&str>,
        at: i64,
    ) -> Result<ReviewerConnectionRow, SqlError> {
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let before = reviewer_connection_from_connection(&transaction, id)?;
        if before.revoked_at.is_some() {
            return Err(StoreError::ReviewerConnectionRevoked(id.to_string()));
        }
        let changed = before.status != status || before.failure_code.as_deref() != failure_code;
        transaction.execute(
            "UPDATE reviewer_connections
             SET status = ?2, failure_code = ?3, lease_expires_at = NULL, updated_at = ?4
             WHERE id = ?1 AND revoked_at IS NULL",
            params![id, status, failure_code, at],
        )?;
        if changed {
            insert_reviewer_connection_event(&transaction, id, status, at)?;
        }
        transaction.commit()?;
        self.reviewer_connection(id)
    }

    /// Persist one CRDT update and its complete semantic provenance atomically.
    pub fn commit_update_with_provenance(
        &self,
        input: &ProvenanceCommitInput,
    ) -> Result<PersistedProvenance, SqlError> {
        validate_event_input(&input.event, &input.anchors)?;
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if let Some(replayed) = idempotent_replay(&transaction, &input.doc_id, &input.event)? {
            transaction.commit()?;
            return Ok(replayed);
        }

        upsert_actor(&transaction, &input.actor)?;
        let update_seq = insert_update(&transaction, &input.doc_id, &input.update)?;
        insert_event(&transaction, &input.doc_id, Some(update_seq), &input.event)?;
        insert_anchors(&transaction, input.event.event_id, &input.anchors)?;
        insert_changes(
            &transaction,
            &input.doc_id,
            input.event.event_id,
            &input.changes,
        )?;
        replace_lineage(
            &transaction,
            &input.doc_id,
            &input.spans,
            &input.lineage,
            update_seq,
            input.event.event_id,
        )?;
        update_block_compatibility(
            &transaction,
            &input.doc_id,
            &input.block_touches,
            &input.current_block_ids,
        )?;
        update_document_projection(
            &transaction,
            &input.doc_id,
            &input.title,
            &input.markdown,
            input.updated_at,
            input.deleted_at,
        )?;
        transaction.commit()?;
        Ok(PersistedProvenance {
            update_seq: Some(update_seq),
            event_id: input.event.event_id,
            replayed: false,
        })
    }

    /// Create a document, its first update, compatibility attribution, and its
    /// complete semantic provenance as one visible operation.
    pub fn create_initial_document_with_provenance(
        &self,
        input: &InitialProvenanceDocumentInput,
    ) -> Result<PersistedProvenance, SqlError> {
        validate_event_input(&input.event, &input.anchors)?;
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if let Some(replayed) = idempotent_replay(&transaction, &input.id, &input.event)? {
            transaction.commit()?;
            return Ok(replayed);
        }

        upsert_actor(&transaction, &input.actor)?;
        transaction.execute(
            "INSERT INTO documents (id, title, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![input.id, input.title, input.created_at, input.updated_at],
        )?;
        let update_seq = insert_update(&transaction, &input.id, &input.update)?;
        insert_event(&transaction, &input.id, Some(update_seq), &input.event)?;
        insert_anchors(&transaction, input.event.event_id, &input.anchors)?;
        insert_changes(
            &transaction,
            &input.id,
            input.event.event_id,
            &input.changes,
        )?;
        replace_lineage(
            &transaction,
            &input.id,
            &input.spans,
            &input.lineage,
            update_seq,
            input.event.event_id,
        )?;
        transaction.execute(
            "INSERT INTO doc_fts (doc_id, title, body) VALUES (?1, ?2, ?3)",
            params![input.id, input.title, input.markdown],
        )?;
        for block_id in &input.block_ids {
            transaction.execute(
                "INSERT INTO block_provenance
                     (doc_id, block_id, created_by, created_at, touched_by, touched_at, session_id)
                 VALUES (?1, ?2, ?3, ?5, ?3, ?5, ?4)",
                params![
                    input.id,
                    block_id,
                    input.update.actor_id,
                    input.update.session_id,
                    input.attributed_at
                ],
            )?;
        }
        transaction.commit()?;
        Ok(PersistedProvenance {
            update_seq: Some(update_seq),
            event_id: input.event.event_id,
            replayed: false,
        })
    }

    /// Append an immutable event without appending a CRDT update.
    ///
    /// Legacy seeds can bind to the document's current final update so the
    /// recorded lineage has an exact replay boundary. Decision-only events can
    /// leave that binding empty.
    pub fn record_provenance_without_update(
        &self,
        input: &ProvenanceRecordInput,
    ) -> Result<PersistedProvenance, SqlError> {
        validate_event_input(&input.event, &input.anchors)?;
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if let Some(replayed) = idempotent_replay(&transaction, &input.doc_id, &input.event)? {
            transaction.commit()?;
            return Ok(replayed);
        }

        let latest_update = latest_update_seq(&transaction, &input.doc_id)?;
        let event_update = input
            .bind_to_latest_update
            .then_some(latest_update)
            .flatten();
        insert_event(&transaction, &input.doc_id, event_update, &input.event)?;
        insert_anchors(&transaction, input.event.event_id, &input.anchors)?;
        insert_changes(
            &transaction,
            &input.doc_id,
            input.event.event_id,
            &input.changes,
        )?;
        replace_lineage(
            &transaction,
            &input.doc_id,
            &input.spans,
            &input.lineage,
            latest_update.unwrap_or(0),
            input.event.event_id,
        )?;
        transaction.commit()?;
        Ok(PersistedProvenance {
            update_seq: event_update,
            event_id: input.event.event_id,
            replayed: false,
        })
    }

    /// Atomically replace only the discardable current-lineage cache.
    pub fn rebuild_lineage_cache(&self, input: &LineageRebuildInput) -> Result<(), SqlError> {
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        replace_lineage(
            &transaction,
            &input.doc_id,
            &input.spans,
            &input.lineage,
            input.through_update_seq,
            input.through_event_id,
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Event envelopes in deterministic document order.
    pub fn provenance_events(&self, doc_id: &str) -> Result<Vec<ProvenanceEventRow>, SqlError> {
        let mut statement = self.conn.prepare(
            "SELECT event_id, doc_id, update_seq, actor_id, action, ingress, assurance,
                    connection_id, session_id, actor_label, source_label, provider, requested_model,
                    reported_model, evidence_ref, suggestion_id, client_event_id,
                    chain_version, before_hash, after_hash, update_log_root,
                    previous_event_hash, event_hash, created_at, recorded_at
             FROM provenance_events WHERE doc_id = ?1 ORDER BY event_id",
        )?;
        let rows = statement
            .query_map(params![doc_id], provenance_event_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Newest event envelope for a document, if it has provenance history.
    pub fn latest_provenance_event(
        &self,
        doc_id: &str,
    ) -> Result<Option<ProvenanceEventRow>, SqlError> {
        Ok(self
            .conn
            .query_row(
                "SELECT event_id, doc_id, update_seq, actor_id, action, ingress, assurance,
                        connection_id, session_id, actor_label, source_label, provider, requested_model,
                        reported_model, evidence_ref, suggestion_id, client_event_id,
                        chain_version, before_hash, after_hash, update_log_root,
                        previous_event_hash, event_hash, created_at, recorded_at
                 FROM provenance_events
                 WHERE doc_id = ?1
                 ORDER BY event_id DESC
                 LIMIT 1",
                params![doc_id],
                provenance_event_from_row,
            )
            .optional()?)
    }

    /// One event's semantic changes in their persisted order.
    pub fn provenance_changes(&self, event_id: i64) -> Result<Vec<ProvenanceChangeRow>, SqlError> {
        let mut statement = self.conn.prepare(
            "SELECT event_id, ordinal, op, source_event_id,
                    before_block_id, before_path, before_from_utf16, before_to_utf16,
                    after_block_id, after_path, after_from_utf16, after_to_utf16,
                    before_text, after_text, before_format, after_format,
                    before_shape, after_shape
             FROM provenance_changes WHERE event_id = ?1 ORDER BY ordinal",
        )?;
        let rows = statement
            .query_map(params![event_id], provenance_change_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// One event's anchored range evidence in its canonical event-local order.
    pub fn provenance_anchors(&self, event_id: i64) -> Result<Vec<ProvenanceAnchorRow>, SqlError> {
        let mut statement = self.conn.prepare(
            "SELECT event_id, ordinal, basis,
                    before_start_grapheme, before_end_grapheme,
                    after_start_grapheme, after_end_grapheme,
                    before_text_hash, after_text_hash
             FROM provenance_anchors WHERE event_id = ?1 ORDER BY ordinal",
        )?;
        let rows = statement
            .query_map(params![event_id], provenance_anchor_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Complete current surviving-lineage read model for one document.
    pub fn lineage_spans(&self, doc_id: &str) -> Result<Vec<LineageSpanRow>, SqlError> {
        let mut statement = self.conn.prepare(
            "SELECT doc_id, block_id, node_path, start_utf16, end_utf16, source_event_id
             FROM lineage_spans WHERE doc_id = ?1
             ORDER BY block_id, node_path, start_utf16",
        )?;
        let rows = statement
            .query_map(params![doc_id], |row| {
                Ok(LineageSpanRow {
                    doc_id: row.get(0)?,
                    span: LineageSpanInput {
                        block_id: row.get(1)?,
                        node_path: row.get(2)?,
                        start_utf16: row.get(3)?,
                        end_utf16: row.get(4)?,
                        source_event_id: row.get(5)?,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// A row exists only when one complete lineage generation is available.
    pub fn lineage_state(&self, doc_id: &str) -> Result<Option<LineageStateRow>, SqlError> {
        Ok(self
            .conn
            .query_row(
                "SELECT doc_id, algorithm_version, through_update_seq,
                        through_event_id, state, lineage_digest, rebuilt_at
                 FROM lineage_state WHERE doc_id = ?1",
                params![doc_id],
                |row| {
                    Ok(LineageStateRow {
                        doc_id: row.get(0)?,
                        algorithm_version: row.get(1)?,
                        through_update_seq: row.get(2)?,
                        through_event_id: row.get(3)?,
                        state: row.get(4)?,
                        lineage_digest: row.get(5)?,
                        rebuilt_at: row.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    /// Full append-only update history with sequence numbers for provenance
    /// hydration and deterministic rebuild.
    pub fn updates_for_rebuild(&self, doc_id: &str) -> Result<Vec<EvidenceUpdate>, SqlError> {
        let mut statement = self.conn.prepare(
            "SELECT seq, payload, actor_id, origin, session_id, created_at
             FROM updates WHERE doc_id = ?1 ORDER BY seq",
        )?;
        Ok(statement
            .query_map(params![doc_id], |row| {
                Ok(EvidenceUpdate {
                    seq: row.get(0)?,
                    payload: row.get(1)?,
                    actor_id: row.get(2)?,
                    origin: row.get(3)?,
                    session_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn upsert_actor(&self, actor: &Actor) -> Result<(), SqlError> {
        upsert_actor(&self.conn, actor)?;
        Ok(())
    }

    /// Legacy low-level helper retained for store tests and compatibility.
    ///
    /// This does not create a provenance event. Production document mutations
    /// must use `thought_mcp::Workspace`, which commits document state and its
    /// evidence ledger atomically.
    pub fn create_document(&self, id: &str, title: &str) -> Result<(), SqlError> {
        let ts = now_ms();
        self.conn.execute(
            "INSERT INTO documents (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![id, title, ts],
        )?;
        Ok(())
    }

    pub fn create_initial_document(&self, document: InitialDocument<'_>) -> Result<(), SqlError> {
        let transaction = self.conn.unchecked_transaction()?;
        let timestamp = now_ms();
        transaction.execute(
            "INSERT INTO documents (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![document.id, document.title, timestamp],
        )?;
        transaction.execute(
            "INSERT INTO updates (doc_id, payload, actor_id, origin, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                document.id,
                document.payload,
                document.actor_id,
                document.origin.as_str(),
                document.session_id,
                timestamp
            ],
        )?;
        transaction.execute(
            "INSERT INTO doc_fts (doc_id, title, body) VALUES (?1, ?2, ?3)",
            params![document.id, document.title, document.markdown],
        )?;
        for block_id in document.block_ids {
            transaction.execute(
                "INSERT INTO block_provenance
                     (doc_id, block_id, created_by, created_at, touched_by, touched_at, session_id)
                 VALUES (?1, ?2, ?3, ?5, ?3, ?5, ?4)",
                params![
                    document.id,
                    block_id,
                    document.actor_id,
                    document.session_id,
                    document.attributed_at
                ],
            )?;
        }
        Ok(transaction.commit()?)
    }

    /// Legacy low-level update helper retained for store tests and compatibility.
    ///
    /// This does not create a provenance event. Production document mutations
    /// must use `thought_mcp::Workspace`, which commits the update, semantic
    /// evidence, and derived projections in one transaction. Callers of this
    /// helper batch an agent turn into a single frame before calling (AD-16).
    pub fn append_update(
        &self,
        doc_id: &str,
        payload: &[u8],
        actor_id: &str,
        origin: Origin,
        session_id: Option<&str>,
    ) -> Result<i64, SqlError> {
        let ts = now_ms();
        self.conn.execute(
            "INSERT INTO updates (doc_id, payload, actor_id, origin, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![doc_id, payload, actor_id, origin.as_str(), session_id, ts],
        )?;
        self.conn.execute(
            "UPDATE documents SET updated_at = ?2 WHERE id = ?1",
            params![doc_id, ts],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Newest snapshot plus every update after it — the cold-start path.
    pub fn restore(&self, doc_id: &str) -> Result<Restored, SqlError> {
        let snapshot: Option<(Vec<u8>, i64)> = self
            .conn
            .query_row(
                "SELECT state, through_seq FROM snapshots
                 WHERE doc_id = ?1 ORDER BY through_seq DESC LIMIT 1",
                params![doc_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let (state, through_seq) = match snapshot {
            Some((state, seq)) => (Some(state), seq),
            None => (None, 0),
        };

        let mut stmt = self
            .conn
            .prepare("SELECT payload FROM updates WHERE doc_id = ?1 AND seq > ?2 ORDER BY seq")?;
        let updates = stmt
            .query_map(params![doc_id, through_seq], |row| row.get::<_, Vec<u8>>(0))?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Restored {
            snapshot: state,
            updates,
            through_seq,
        })
    }

    /// The full log, for the activity feed and per-run revert.
    pub fn log(&self, doc_id: &str) -> Result<Vec<LoggedUpdate>, SqlError> {
        self.updates_for_rebuild(doc_id)?
            .into_iter()
            .map(|update| {
                let origin = Origin::parse(&update.origin).ok_or_else(|| {
                    StoreError::InvalidStoredOrigin {
                        seq: update.seq,
                        value: update.origin.clone(),
                    }
                })?;
                Ok(LoggedUpdate {
                    seq: update.seq,
                    payload: update.payload,
                    actor_id: update.actor_id,
                    origin,
                    session_id: update.session_id,
                    created_at: update.created_at,
                })
            })
            .collect()
    }

    pub fn updates_since_snapshot(&self, doc_id: &str) -> Result<i64, SqlError> {
        let through: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(through_seq), 0) FROM snapshots WHERE doc_id = ?1",
                params![doc_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM updates WHERE doc_id = ?1 AND seq > ?2",
            params![doc_id, through],
            |row| row.get(0),
        )?)
    }

    /// Record a compacted snapshot. Keeps the two newest and drops older ones —
    /// **and never touches `updates`**, which is a different retention policy
    /// serving a different purpose (AD-13).
    pub fn write_snapshot(
        &self,
        doc_id: &str,
        through_seq: i64,
        state: &[u8],
        state_vector: &[u8],
    ) -> Result<(), SqlError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO snapshots (doc_id, through_seq, state, state_vector, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![doc_id, through_seq, state, state_vector, now_ms()],
        )?;
        self.conn.execute(
            "DELETE FROM snapshots WHERE doc_id = ?1 AND through_seq NOT IN
             (SELECT through_seq FROM snapshots WHERE doc_id = ?1
              ORDER BY through_seq DESC LIMIT 2)",
            params![doc_id],
        )?;
        Ok(())
    }

    /// Refresh the search index from the markdown projection. Rewritten on
    /// snapshot rather than per update: agents need search, but not so fresh
    /// that every keystroke reindexes a document.
    pub fn reindex(&self, doc_id: &str, title: &str, body: &str) -> Result<(), SqlError> {
        self.conn
            .execute("DELETE FROM doc_fts WHERE doc_id = ?1", params![doc_id])?;
        self.conn.execute(
            "INSERT INTO doc_fts (doc_id, title, body) VALUES (?1, ?2, ?3)",
            params![doc_id, title, body],
        )?;
        self.conn.execute(
            "UPDATE documents SET title = ?2 WHERE id = ?1",
            params![doc_id, title],
        )?;
        Ok(())
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<(String, String)>, SqlError> {
        Ok(self
            .search_scoped(query, limit, None)?
            .into_iter()
            .map(|(doc_id, _title, snippet)| (doc_id, snippet))
            .collect())
    }

    /// Search a bounded set directly in SQLite. A selected-document reviewer
    /// must not scan unrelated private documents and filter them afterward.
    pub fn search_scoped(
        &self,
        query: &str,
        limit: usize,
        document_id: Option<&str>,
    ) -> Result<Vec<(String, String, String)>, SqlError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = if let Some(document_id) = document_id {
            let mut stmt = self.conn.prepare(
                "SELECT doc_id, title, snippet(doc_fts, 2, '<b>', '</b>', '…', 12)
                 FROM doc_fts
                 WHERE doc_fts MATCH ?1 AND doc_id = ?2
                 LIMIT ?3",
            )?;
            stmt.query_map(params![query, document_id, limit], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT doc_id, title, snippet(doc_fts, 2, '<b>', '</b>', '…', 12)
                 FROM doc_fts WHERE doc_fts MATCH ?1 LIMIT ?2",
            )?;
            stmt.query_map(params![query, limit], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    /// Documents, most recently updated first.
    ///
    /// `trashed` selects which side of the tombstone to look at. Deleting is
    /// soft (AD-14), so the rows never leave — but without this there was no way
    /// back to a document once deleted, which makes "soft" a claim rather than a
    /// feature.
    pub fn list_documents(&self, trashed: bool) -> Result<Vec<DocumentRow>, SqlError> {
        self.list_documents_scoped(trashed, usize::MAX, None)
    }

    /// List a bounded set directly in SQLite, optionally constrained to one
    /// selected document before ordering and limiting.
    pub fn list_documents_scoped(
        &self,
        trashed: bool,
        limit: usize,
        document_id: Option<&str>,
    ) -> Result<Vec<DocumentRow>, SqlError> {
        let deleted_predicate = if trashed {
            "deleted_at IS NOT NULL"
        } else {
            "deleted_at IS NULL"
        };
        let order = if trashed {
            "deleted_at DESC"
        } else {
            "updated_at DESC"
        };
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = if let Some(document_id) = document_id {
            let sql = format!(
                "SELECT id, title, updated_at, deleted_at FROM documents
                 WHERE {deleted_predicate} AND id = ?1 ORDER BY {order} LIMIT ?2"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            stmt.query_map(params![document_id, limit], document_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let sql = format!(
                "SELECT id, title, updated_at, deleted_at FROM documents
                 WHERE {deleted_predicate} ORDER BY {order} LIMIT ?1"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            stmt.query_map(params![limit], document_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    /// Everyone who has written to a document, most recent first.
    ///
    /// A summary has one model slot, so it is populated only when every update
    /// by that actor has exactly one immutable event and all those events report
    /// the same model. Mutable actor metadata never supplies historical proof.
    pub fn actors_for_document(&self, doc_id: &str) -> Result<Vec<ActorActivity>, SqlError> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.kind, a.display_name,
                    CASE
                      WHEN COUNT(u.seq) = COUNT(e.event_id)
                       AND COUNT(u.seq) = COUNT(e.reported_model)
                       AND COUNT(DISTINCT e.reported_model) = 1
                      THEN MAX(e.reported_model)
                      ELSE NULL
                    END AS model,
                    a.color,
                    MAX(u.created_at) AS last_seen, COUNT(*) AS edits
             FROM updates u
             JOIN actors a ON a.id = u.actor_id
             LEFT JOIN provenance_events e
               ON e.doc_id = u.doc_id
              AND e.update_seq = u.seq
              AND e.actor_id = u.actor_id
              AND e.session_id IS u.session_id
              AND e.created_at = u.created_at
             WHERE u.doc_id = ?1
             GROUP BY a.id
             ORDER BY last_seen DESC",
        )?;
        let rows = stmt
            .query_map(params![doc_id], |row| {
                Ok(ActorActivity {
                    actor_id: row.get(0)?,
                    kind: row.get(1)?,
                    display_name: row.get(2)?,
                    model: row.get(3)?,
                    color: row.get(4)?,
                    last_seen: row.get(5)?,
                    edits: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Record that `actor_id` touched `block_id`. First touch also sets
    /// `created_by`, and later ones deliberately leave it alone.
    pub fn touch_block(
        &self,
        doc_id: &str,
        block_id: &str,
        actor_id: &str,
        session_id: Option<&str>,
        at: i64,
    ) -> Result<(), SqlError> {
        self.conn.execute(
            "INSERT INTO block_provenance
                 (doc_id, block_id, created_by, created_at, touched_by, touched_at, session_id)
             VALUES (?1, ?2, ?3, ?5, ?3, ?5, ?4)
             ON CONFLICT(doc_id, block_id) DO UPDATE SET touched_by = excluded.touched_by,
                                                         touched_at = excluded.touched_at,
                                                         session_id = excluded.session_id",
            params![doc_id, block_id, actor_id, session_id, at],
        )?;
        Ok(())
    }

    /// Drop rows for blocks that no longer exist, so the table tracks the
    /// document rather than growing with every paragraph ever deleted.
    ///
    /// Reads the current ids and deletes the difference rather than building an
    /// `IN (...)` list: block ids are interpolated from document content, and
    /// SQL assembled by string concatenation is how that becomes an injection.
    pub fn forget_blocks(&self, doc_id: &str, keep: &HashSet<String>) -> Result<(), SqlError> {
        let mut stmt = self
            .conn
            .prepare("SELECT block_id FROM block_provenance WHERE doc_id = ?1")?;
        let present = stmt
            .query_map(params![doc_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for block_id in present.iter().filter(|id| !keep.contains(*id)) {
            self.conn.execute(
                "DELETE FROM block_provenance WHERE doc_id = ?1 AND block_id = ?2",
                params![doc_id, block_id],
            )?;
        }
        Ok(())
    }

    /// True once a document has been attributed, so the rebuild-by-replay path
    /// runs once per document rather than on every hydration.
    pub fn has_provenance(&self, doc_id: &str) -> Result<bool, SqlError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM block_provenance WHERE doc_id = ?1",
            params![doc_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Every attributed block in a document, joined to whoever last touched it.
    ///
    /// The optional model comes from the single immutable event that can be
    /// tied to the last-touch update. Timestamp or session collisions make that
    /// attribution ambiguous, in which case no model is returned.
    pub fn provenance_for_document(&self, doc_id: &str) -> Result<Vec<BlockAttribution>, SqlError> {
        let mut stmt = self.conn.prepare(
            "SELECT p.block_id, p.created_by, p.created_at, p.touched_by, p.touched_at,
                    p.session_id, a.kind, a.display_name, h.reported_model, a.color
             FROM block_provenance p
             JOIN actors a ON a.id = p.touched_by
             LEFT JOIN (
               SELECT u.doc_id, u.actor_id, u.session_id, u.created_at,
                      CASE WHEN COUNT(*) = 1 THEN MAX(e.reported_model) ELSE NULL END
                        AS reported_model
               FROM updates u
               JOIN provenance_events e
                 ON e.doc_id = u.doc_id
                AND e.update_seq = u.seq
                AND e.actor_id = u.actor_id
                AND e.session_id IS u.session_id
                AND e.created_at = u.created_at
               WHERE u.doc_id = ?1
               GROUP BY u.doc_id, u.actor_id, u.session_id, u.created_at
             ) h
               ON h.doc_id = p.doc_id
              AND h.actor_id = p.touched_by
              AND h.session_id IS p.session_id
              AND h.created_at = p.touched_at
             WHERE p.doc_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![doc_id], |row| {
                Ok(BlockAttribution {
                    block_id: row.get(0)?,
                    created_by: row.get(1)?,
                    created_at: row.get(2)?,
                    touched_by: row.get(3)?,
                    touched_at: row.get(4)?,
                    session_id: row.get(5)?,
                    kind: row.get(6)?,
                    display_name: row.get(7)?,
                    model: row.get(8)?,
                    color: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Cache of the tombstone that actually lives in the document CRDT (AD-14).
    /// A column cannot replicate, so this is derived state, never the source.
    pub fn cache_deleted_at(&self, doc_id: &str, at: Option<i64>) -> Result<(), SqlError> {
        self.conn.execute(
            "UPDATE documents SET deleted_at = ?2 WHERE id = ?1",
            params![doc_id, at],
        )?;
        Ok(())
    }
}

fn validate_event_input(
    event: &ProvenanceEventInput,
    anchors: &[ProvenanceAnchorInput],
) -> Result<(), StoreError> {
    if event.event_id <= 0 {
        return Err(StoreError::InvalidEventId(event.event_id));
    }

    let invalid = |reason: String| StoreError::InvalidProvenanceAnchors {
        event_id: event.event_id,
        reason,
    };
    match event.chain_version {
        1 if !anchors.is_empty() => {
            return Err(invalid("chain version 1 does not admit anchors".into()));
        }
        1 => return Ok(()),
        2 if anchors.is_empty() => {
            return Err(invalid(
                "chain version 2 requires at least one anchor".into(),
            ));
        }
        2 => {}
        version => {
            return Err(invalid(format!(
                "unsupported chain version {version}; expected 1 or 2"
            )));
        }
    }

    for (ordinal, anchor) in anchors.iter().enumerate() {
        if !matches!(
            anchor.basis.as_str(),
            "editor_transaction" | "server_operation"
        ) {
            return Err(invalid(format!(
                "anchor {ordinal} has unsupported basis `{}`",
                anchor.basis
            )));
        }
        if anchor.before_start_grapheme < 0
            || anchor.before_end_grapheme < anchor.before_start_grapheme
        {
            return Err(invalid(format!(
                "anchor {ordinal} has invalid before range {}..{}",
                anchor.before_start_grapheme, anchor.before_end_grapheme
            )));
        }
        if anchor.after_start_grapheme < 0
            || anchor.after_end_grapheme < anchor.after_start_grapheme
        {
            return Err(invalid(format!(
                "anchor {ordinal} has invalid after range {}..{}",
                anchor.after_start_grapheme, anchor.after_end_grapheme
            )));
        }
        if anchor.before_text_hash.len() != 32 {
            return Err(invalid(format!(
                "anchor {ordinal} before-text hash is {} bytes, expected 32",
                anchor.before_text_hash.len()
            )));
        }
        if anchor.after_text_hash.len() != 32 {
            return Err(invalid(format!(
                "anchor {ordinal} after-text hash is {} bytes, expected 32",
                anchor.after_text_hash.len()
            )));
        }
        if let Some(previous) = ordinal.checked_sub(1).map(|index| &anchors[index])
            && (previous.before_end_grapheme > anchor.before_start_grapheme
                || previous.after_end_grapheme > anchor.after_start_grapheme)
        {
            return Err(invalid(format!(
                "anchor {ordinal} overlaps or precedes anchor {}",
                ordinal - 1
            )));
        }
    }
    Ok(())
}

fn idempotent_replay(
    transaction: &Transaction<'_>,
    doc_id: &str,
    event: &ProvenanceEventInput,
) -> Result<Option<PersistedProvenance>, StoreError> {
    let Some(client_event_id) = event.client_event_id.as_deref() else {
        return Ok(None);
    };
    let existing: Option<(i64, Option<i64>, Vec<u8>)> = transaction
        .query_row(
            "SELECT event_id, update_seq, event_hash
             FROM provenance_events
             WHERE doc_id = ?1 AND client_event_id = ?2",
            params![doc_id, client_event_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((event_id, update_seq, event_hash)) = existing else {
        return Ok(None);
    };
    if event_id != event.event_id || event_hash != event.event_hash {
        return Err(StoreError::IdempotencyConflict {
            doc_id: doc_id.to_string(),
            client_event_id: client_event_id.to_string(),
        });
    }
    Ok(Some(PersistedProvenance {
        update_seq,
        event_id,
        replayed: true,
    }))
}

fn insert_update(
    transaction: &Transaction<'_>,
    doc_id: &str,
    update: &ProvenanceUpdateInput,
) -> Result<i64, StoreError> {
    if update.expected_seq <= 0 {
        return Err(StoreError::UnexpectedUpdateSequence {
            expected: update.expected_seq,
            actual: 0,
        });
    }
    transaction.execute(
        "INSERT INTO updates (seq, doc_id, payload, actor_id, origin, session_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            update.expected_seq,
            doc_id,
            update.payload,
            update.actor_id,
            update.origin.as_str(),
            update.session_id,
            update.created_at
        ],
    )?;
    let actual = transaction.last_insert_rowid();
    if actual != update.expected_seq {
        return Err(StoreError::UnexpectedUpdateSequence {
            expected: update.expected_seq,
            actual,
        });
    }
    Ok(actual)
}

fn insert_event(
    transaction: &Transaction<'_>,
    doc_id: &str,
    update_seq: Option<i64>,
    event: &ProvenanceEventInput,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO provenance_events (
             event_id, doc_id, update_seq, actor_id, action, ingress, assurance,
             connection_id, session_id, actor_label, source_label, provider, requested_model,
             reported_model, evidence_ref, suggestion_id, client_event_id,
             chain_version, before_hash, after_hash, update_log_root,
             previous_event_hash, event_hash, created_at, recorded_at
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
             ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24,
             ?25
         )",
        params![
            event.event_id,
            doc_id,
            update_seq,
            event.actor_id,
            event.action,
            event.ingress,
            event.assurance,
            event.connection_id,
            event.session_id,
            event.actor_label,
            event.source_label,
            event.provider,
            event.requested_model,
            event.reported_model,
            event.evidence_ref,
            event.suggestion_id,
            event.client_event_id,
            event.chain_version,
            event.before_hash,
            event.after_hash,
            event.update_log_root,
            event.previous_event_hash,
            event.event_hash,
            event.created_at,
            event.recorded_at
        ],
    )?;
    Ok(())
}

fn insert_anchors(
    transaction: &Transaction<'_>,
    event_id: i64,
    anchors: &[ProvenanceAnchorInput],
) -> Result<(), StoreError> {
    let mut statement = transaction.prepare_cached(
        "INSERT INTO provenance_anchors (
             event_id, ordinal, basis,
             before_start_grapheme, before_end_grapheme,
             after_start_grapheme, after_end_grapheme,
             before_text_hash, after_text_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for (ordinal, anchor) in anchors.iter().enumerate() {
        statement.execute(params![
            event_id,
            ordinal as i64,
            anchor.basis,
            anchor.before_start_grapheme,
            anchor.before_end_grapheme,
            anchor.after_start_grapheme,
            anchor.after_end_grapheme,
            anchor.before_text_hash,
            anchor.after_text_hash,
        ])?;
    }
    Ok(())
}

fn insert_changes(
    transaction: &Transaction<'_>,
    doc_id: &str,
    event_id: i64,
    changes: &[ProvenanceChangeInput],
) -> Result<(), StoreError> {
    let source_ids = changes
        .iter()
        .filter_map(|change| change.source_event_id)
        .collect::<HashSet<_>>();
    for source_event_id in source_ids {
        if source_event_id > event_id {
            return Err(StoreError::ProvenanceChangeSourceInFuture {
                event_id,
                source_event_id,
            });
        }
        let source_doc_id: Option<String> = transaction
            .query_row(
                "SELECT doc_id FROM provenance_events WHERE event_id = ?1",
                params![source_event_id],
                |row| row.get(0),
            )
            .optional()?;
        if source_doc_id.as_deref() != Some(doc_id) {
            return Err(StoreError::ProvenanceChangeSourceMismatch {
                doc_id: doc_id.to_string(),
                event_id,
                source_event_id,
            });
        }
    }

    let mut statement = transaction.prepare_cached(
        "INSERT INTO provenance_changes (
             event_id, ordinal, op, source_event_id,
             before_block_id, before_path, before_from_utf16, before_to_utf16,
             after_block_id, after_path, after_from_utf16, after_to_utf16,
             before_text, after_text, before_format, after_format,
             before_shape, after_shape
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
             ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
         )",
    )?;
    for (ordinal, change) in changes.iter().enumerate() {
        statement.execute(params![
            event_id,
            ordinal as i64,
            change.op,
            change.source_event_id,
            change.before_block_id,
            change.before_path,
            change.before_from_utf16,
            change.before_to_utf16,
            change.after_block_id,
            change.after_path,
            change.after_from_utf16,
            change.after_to_utf16,
            change.before_text,
            change.after_text,
            change.before_format,
            change.after_format,
            change.before_shape,
            change.after_shape
        ])?;
    }
    Ok(())
}

fn upsert_actor(connection: &Connection, actor: &Actor) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO actors (id, kind, display_name, model, color, first_seen)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET display_name = excluded.display_name,
                                       model        = excluded.model,
                                       color        = excluded.color",
        params![
            actor.id,
            actor.kind,
            actor.display_name,
            actor.model,
            actor.color,
            now_ms()
        ],
    )?;
    Ok(())
}

fn replace_lineage(
    transaction: &Transaction<'_>,
    doc_id: &str,
    spans: &[LineageSpanInput],
    lineage: &ReadyLineageInput,
    through_update_seq: i64,
    through_event_id: i64,
) -> Result<(), StoreError> {
    let source_ids = spans
        .iter()
        .map(|span| span.source_event_id)
        .collect::<HashSet<_>>();
    for source_event_id in source_ids {
        let belongs: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM provenance_events
                 WHERE doc_id = ?1 AND event_id = ?2
             )",
            params![doc_id, source_event_id],
            |row| row.get(0),
        )?;
        if !belongs {
            return Err(StoreError::LineageSourceMismatch {
                doc_id: doc_id.to_string(),
                source_event_id,
            });
        }
    }

    transaction.execute(
        "DELETE FROM lineage_spans WHERE doc_id = ?1",
        params![doc_id],
    )?;
    let mut statement = transaction.prepare_cached(
        "INSERT INTO lineage_spans (
             doc_id, block_id, node_path, start_utf16, end_utf16, source_event_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for span in spans {
        statement.execute(params![
            doc_id,
            span.block_id,
            span.node_path,
            span.start_utf16,
            span.end_utf16,
            span.source_event_id
        ])?;
    }
    drop(statement);

    transaction.execute(
        "INSERT INTO lineage_state (
             doc_id, algorithm_version, through_update_seq, through_event_id,
             state, lineage_digest, rebuilt_at
         ) VALUES (?1, ?2, ?3, ?4, 'ready', ?5, ?6)
         ON CONFLICT(doc_id) DO UPDATE SET
             algorithm_version = excluded.algorithm_version,
             through_update_seq = excluded.through_update_seq,
             through_event_id = excluded.through_event_id,
             state = excluded.state,
             lineage_digest = excluded.lineage_digest,
             rebuilt_at = excluded.rebuilt_at",
        params![
            doc_id,
            lineage.algorithm_version,
            through_update_seq,
            through_event_id,
            lineage.lineage_digest,
            lineage.rebuilt_at
        ],
    )?;
    Ok(())
}

fn update_block_compatibility(
    transaction: &Transaction<'_>,
    doc_id: &str,
    touches: &[BlockTouchInput],
    current_block_ids: &[String],
) -> Result<(), StoreError> {
    for touch in touches {
        transaction.execute(
            "INSERT INTO block_provenance
                 (doc_id, block_id, created_by, created_at, touched_by, touched_at, session_id)
             VALUES (?1, ?2, ?3, ?5, ?3, ?5, ?4)
             ON CONFLICT(doc_id, block_id) DO UPDATE SET
                 touched_by = excluded.touched_by,
                 touched_at = excluded.touched_at,
                 session_id = excluded.session_id",
            params![
                doc_id,
                touch.block_id,
                touch.actor_id,
                touch.session_id,
                touch.at
            ],
        )?;
    }

    let keep = current_block_ids.iter().collect::<HashSet<_>>();
    let mut statement =
        transaction.prepare("SELECT block_id FROM block_provenance WHERE doc_id = ?1")?;
    let present = statement
        .query_map(params![doc_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for block_id in present.iter().filter(|block_id| !keep.contains(block_id)) {
        transaction.execute(
            "DELETE FROM block_provenance WHERE doc_id = ?1 AND block_id = ?2",
            params![doc_id, block_id],
        )?;
    }
    Ok(())
}

fn update_document_projection(
    transaction: &Transaction<'_>,
    doc_id: &str,
    title: &str,
    markdown: &str,
    updated_at: i64,
    deleted_at: Option<i64>,
) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE documents
         SET title = ?2, updated_at = ?3, deleted_at = ?4
         WHERE id = ?1",
        params![doc_id, title, updated_at, deleted_at],
    )?;
    transaction.execute("DELETE FROM doc_fts WHERE doc_id = ?1", params![doc_id])?;
    transaction.execute(
        "INSERT INTO doc_fts (doc_id, title, body) VALUES (?1, ?2, ?3)",
        params![doc_id, title, markdown],
    )?;
    Ok(())
}

fn latest_update_seq(
    transaction: &Transaction<'_>,
    doc_id: &str,
) -> Result<Option<i64>, StoreError> {
    Ok(transaction.query_row(
        "SELECT MAX(seq) FROM updates WHERE doc_id = ?1",
        params![doc_id],
        |row| row.get(0),
    )?)
}

fn provenance_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProvenanceEventRow> {
    Ok(ProvenanceEventRow {
        event_id: row.get(0)?,
        doc_id: row.get(1)?,
        update_seq: row.get(2)?,
        actor_id: row.get(3)?,
        action: row.get(4)?,
        ingress: row.get(5)?,
        assurance: row.get(6)?,
        connection_id: row.get(7)?,
        session_id: row.get(8)?,
        actor_label: row.get(9)?,
        source_label: row.get(10)?,
        provider: row.get(11)?,
        requested_model: row.get(12)?,
        reported_model: row.get(13)?,
        evidence_ref: row.get(14)?,
        suggestion_id: row.get(15)?,
        client_event_id: row.get(16)?,
        chain_version: row.get(17)?,
        before_hash: row.get(18)?,
        after_hash: row.get(19)?,
        update_log_root: row.get(20)?,
        previous_event_hash: row.get(21)?,
        event_hash: row.get(22)?,
        created_at: row.get(23)?,
        recorded_at: row.get(24)?,
    })
}

fn provenance_change_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProvenanceChangeRow> {
    Ok(ProvenanceChangeRow {
        event_id: row.get(0)?,
        ordinal: row.get(1)?,
        change: ProvenanceChangeInput {
            op: row.get(2)?,
            source_event_id: row.get(3)?,
            before_block_id: row.get(4)?,
            before_path: row.get(5)?,
            before_from_utf16: row.get(6)?,
            before_to_utf16: row.get(7)?,
            after_block_id: row.get(8)?,
            after_path: row.get(9)?,
            after_from_utf16: row.get(10)?,
            after_to_utf16: row.get(11)?,
            before_text: row.get(12)?,
            after_text: row.get(13)?,
            before_format: row.get(14)?,
            after_format: row.get(15)?,
            before_shape: row.get(16)?,
            after_shape: row.get(17)?,
        },
    })
}

fn provenance_anchor_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProvenanceAnchorRow> {
    Ok(ProvenanceAnchorRow {
        event_id: row.get(0)?,
        ordinal: row.get(1)?,
        anchor: ProvenanceAnchorInput {
            basis: row.get(2)?,
            before_start_grapheme: row.get(3)?,
            before_end_grapheme: row.get(4)?,
            after_start_grapheme: row.get(5)?,
            after_end_grapheme: row.get(6)?,
            before_text_hash: row.get(7)?,
            after_text_hash: row.get(8)?,
        },
    })
}

fn validate_new_reviewer_connection(
    input: &NewReviewerConnectionInput<'_>,
) -> Result<(), StoreError> {
    if !matches!(
        input.client,
        "chatgpt" | "codex" | "claude_desktop" | "claude_code"
    ) {
        return Err(StoreError::InvalidReviewerConnectionTransition(format!(
            "unknown reviewer client `{}`",
            input.client
        )));
    }
    let expected_provider = match input.client {
        "chatgpt" | "codex" => "openai",
        "claude_desktop" | "claude_code" => "anthropic",
        _ => unreachable!(),
    };
    if input.provider != expected_provider {
        return Err(StoreError::InvalidReviewerConnectionTransition(format!(
            "client `{}` requires provider `{expected_provider}`",
            input.client
        )));
    }
    if input.credential_hash.len() != 32 {
        return Err(StoreError::InvalidReviewerConnectionTransition(
            "credential hashes must contain 32 bytes".to_string(),
        ));
    }
    validate_reviewer_management_state(
        input.display_label,
        input.document_scope,
        input.can_create,
        input.document_ids,
    )
}

fn validate_reviewer_management_state(
    display_label: &str,
    document_scope: &str,
    can_create: bool,
    document_ids: &[String],
) -> Result<(), StoreError> {
    let label_length = display_label.trim().chars().count();
    if !(1..=80).contains(&label_length) {
        return Err(StoreError::InvalidReviewerConnectionTransition(
            "reviewer labels must contain 1 to 80 characters".to_string(),
        ));
    }
    if !matches!(document_scope, "selected" | "all") {
        return Err(StoreError::InvalidReviewerConnectionTransition(format!(
            "unknown document scope `{document_scope}`"
        )));
    }
    if document_scope == "selected" && document_ids.is_empty() {
        return Err(StoreError::InvalidReviewerConnectionTransition(
            "selected-document access requires at least one document".to_string(),
        ));
    }
    if document_scope == "all" && !document_ids.is_empty() {
        return Err(StoreError::InvalidReviewerConnectionTransition(
            "all-document access cannot include a selected-document list".to_string(),
        ));
    }
    if can_create && document_scope != "all" {
        return Err(StoreError::InvalidReviewerConnectionTransition(
            "creating documents requires all-document access".to_string(),
        ));
    }
    Ok(())
}

fn normalized_document_ids(document_scope: &str, document_ids: &[String]) -> Vec<String> {
    if document_scope == "all" {
        return Vec::new();
    }
    let mut ids = document_ids.to_vec();
    ids.sort();
    ids.dedup();
    ids
}

fn replace_reviewer_documents(
    transaction: &Transaction<'_>,
    connection_id: &str,
    document_ids: &[String],
    created_at: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM reviewer_connection_documents WHERE connection_id = ?1",
        params![connection_id],
    )?;
    let mut statement = transaction.prepare_cached(
        "INSERT INTO reviewer_connection_documents (connection_id, doc_id, created_at)
         VALUES (?1, ?2, ?3)",
    )?;
    for document_id in document_ids {
        statement.execute(params![connection_id, document_id, created_at])?;
    }
    Ok(())
}

fn ensure_reviewer_mutable(
    connection: &ReviewerConnectionRow,
    expected_revision: i64,
) -> Result<(), StoreError> {
    if connection.revoked_at.is_some() {
        return Err(StoreError::ReviewerConnectionRevoked(connection.id.clone()));
    }
    if connection.revision != expected_revision {
        return Err(StoreError::ReviewerConnectionRevisionConflict {
            id: connection.id.clone(),
            expected: expected_revision,
            actual: connection.revision,
        });
    }
    Ok(())
}

fn reviewer_revision_error(
    connection: &Connection,
    id: &str,
    expected_revision: i64,
) -> StoreError {
    match reviewer_connection_from_connection(connection, id) {
        Ok(row) if row.revoked_at.is_some() => {
            StoreError::ReviewerConnectionRevoked(id.to_string())
        }
        Ok(row) => StoreError::ReviewerConnectionRevisionConflict {
            id: id.to_string(),
            expected: expected_revision,
            actual: row.revision,
        },
        Err(error) => error,
    }
}

fn reviewer_connection_from_connection(
    connection: &Connection,
    id: &str,
) -> Result<ReviewerConnectionRow, StoreError> {
    let mut row = connection
        .query_row(
            "SELECT id, client, provider, display_label, status, document_scope,
                    can_read, can_edit, can_create, can_trash, credential_hash,
                    pending_credential_hash, credential_version, revision,
                    reported_model, failure_code, created_at, updated_at,
                    first_connected_at, last_seen_at, lease_expires_at,
                    credential_expires_at, revoked_at
             FROM reviewer_connections WHERE id = ?1",
            params![id],
            reviewer_connection_from_row,
        )
        .optional()?
        .ok_or_else(|| StoreError::ReviewerConnectionNotFound(id.to_string()))?;
    let mut statement = connection.prepare(
        "SELECT doc_id FROM reviewer_connection_documents
         WHERE connection_id = ?1 ORDER BY doc_id",
    )?;
    row.document_ids = statement
        .query_map(params![id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(row)
}

fn reviewer_connection_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ReviewerConnectionRow> {
    Ok(ReviewerConnectionRow {
        id: row.get(0)?,
        client: row.get(1)?,
        provider: row.get(2)?,
        display_label: row.get(3)?,
        status: row.get(4)?,
        document_scope: row.get(5)?,
        can_read: row.get(6)?,
        can_edit: row.get(7)?,
        can_create: row.get(8)?,
        can_trash: row.get(9)?,
        credential_hash: row.get(10)?,
        pending_credential_hash: row.get(11)?,
        credential_version: row.get(12)?,
        revision: row.get(13)?,
        reported_model: row.get(14)?,
        failure_code: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
        first_connected_at: row.get(18)?,
        last_seen_at: row.get(19)?,
        lease_expires_at: row.get(20)?,
        credential_expires_at: row.get(21)?,
        revoked_at: row.get(22)?,
        document_ids: Vec::new(),
    })
}

fn insert_reviewer_connection_event(
    transaction: &Transaction<'_>,
    connection_id: &str,
    event_type: &str,
    created_at: i64,
) -> Result<(), StoreError> {
    let inserted = transaction.execute(
        "INSERT INTO reviewer_connection_events (
             connection_id, revision, event_type, display_label, status,
             document_scope, can_read, can_edit, can_create, can_trash,
             document_ids_json, failure_code, created_at
         )
         SELECT id, revision, ?2, display_label, status, document_scope,
                can_read, can_edit, can_create, can_trash,
                COALESCE((
                  SELECT json_group_array(doc_id)
                  FROM (
                    SELECT doc_id FROM reviewer_connection_documents
                    WHERE connection_id = ?1 ORDER BY doc_id
                  )
                ), '[]'),
                failure_code, ?3
         FROM reviewer_connections WHERE id = ?1",
        params![connection_id, event_type, created_at],
    )?;
    if inserted != 1 {
        return Err(StoreError::ReviewerConnectionNotFound(
            connection_id.to_string(),
        ));
    }
    Ok(())
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
