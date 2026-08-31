//! Durable storage: an append-only op log, periodic snapshots, and the actors
//! that wrote them (M1.4).
//!
//! Deliberately ignorant of CRDT semantics — it stores opaque update frames.
//! Two retention policies live here and they are not the same policy:
//! snapshots exist for *load performance* and may be discarded freely, while
//! the op log exists for *provenance* and is never compacted away, because the
//! activity feed and per-run revert read it (AD-13).

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use std::collections::HashSet;
use std::path::Path;

mod schema;

pub use rusqlite::Error as SqlError;

/// A read-only startup decision. Only accepted version 0 and exact current
/// version 7 stores may be opened by this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreCompatibility {
    Missing,
    Current,
    Unsupported,
}

/// Inspect an existing store without creating it or running persistent
/// pragmas or schema DDL. A missing path is reported separately so callers
/// deciding whether to stop a published daemon cannot treat absence as safe.
pub fn inspect_compatibility(path: impl AsRef<Path>) -> Result<StoreCompatibility, SqlError> {
    let path = path.as_ref();
    if !path
        .try_exists()
        .map_err(|_| SqlError::InvalidPath(path.to_path_buf()))?
    {
        return Ok(StoreCompatibility::Missing);
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    classify_connection(&connection)
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
    fn as_str(self) -> &'static str {
        match self {
            Origin::Human => "human",
            Origin::Agent => "agent",
            Origin::Remote => "remote",
        }
    }

    fn parse(s: &str) -> Origin {
        match s {
            "agent" => Origin::Agent,
            "remote" => Origin::Remote,
            _ => Origin::Human,
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
#[derive(Debug, Clone)]
pub struct LoggedUpdate {
    pub seq: i64,
    pub payload: Vec<u8>,
    pub actor_id: String,
    pub origin: Origin,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerConnectionRow {
    pub id: String,
    pub client: String,
    pub display_label: String,
    pub document_scope: String,
    pub document_id: Option<String>,
    pub credential_hash: [u8; 32],
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_seen_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub reported_model: Option<String>,
}

pub struct NewReviewerConnection<'a> {
    pub id: &'a str,
    pub client: &'a str,
    pub display_label: &'a str,
    pub document_scope: &'a str,
    pub document_id: Option<&'a str>,
    pub credential_hash: &'a [u8; 32],
    pub created_at: i64,
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

#[derive(Debug, Clone)]
pub struct ProvenanceEventRow {
    pub event_id: i64,
    pub actor_id: Option<String>,
    pub action: String,
    pub group_key: String,
    pub source_label: String,
    pub ingress: String,
    pub assurance: String,
    pub alignment: String,
    pub session_id: Option<String>,
    pub created_at: i64,
}

pub struct ProvenanceEventInput<'a> {
    pub event_id: i64,
    pub actor_id: Option<&'a str>,
    pub action: &'a str,
    pub group_key: &'a str,
    pub source_label: &'a str,
    pub ingress: &'a str,
    pub assurance: &'a str,
    pub alignment: &'a str,
    pub session_id: Option<&'a str>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageSpanRow {
    pub block_id: String,
    pub node_path: String,
    pub start_utf16: i64,
    pub end_utf16: i64,
    pub source_event_id: i64,
}

pub struct LineageUpdate<'a> {
    pub doc_id: &'a str,
    pub expected_seq: i64,
    pub payload: &'a [u8],
    pub actor_id: &'a str,
    pub origin: Origin,
    pub session_id: Option<&'a str>,
    pub title: &'a str,
    pub markdown: &'a str,
    pub deleted_at: Option<i64>,
    pub touched_blocks: &'a [String],
    pub current_blocks: &'a [String],
    pub event: ProvenanceEventInput<'a>,
    pub spans: &'a [LineageSpanRow],
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

#[derive(Debug, PartialEq, Eq)]
struct SchemaObject {
    object_type: String,
    name: String,
    table_name: String,
    sql: Option<String>,
}

fn schema_objects(connection: &Connection) -> Result<Vec<SchemaObject>, SqlError> {
    let mut statement = connection.prepare(
        "SELECT type, name, tbl_name, sql
         FROM sqlite_schema
         WHERE name NOT GLOB 'sqlite_*'
         ORDER BY type, name, tbl_name",
    )?;
    statement
        .query_map([], |row| {
            Ok(SchemaObject {
                object_type: row.get(0)?,
                name: row.get(1)?,
                table_name: row.get(2)?,
                sql: row.get(3)?,
            })
        })?
        .collect()
}

fn expected_schema_objects() -> Result<Vec<SchemaObject>, SqlError> {
    let reference = Connection::open_in_memory()?;
    reference.execute_batch(schema::SCHEMA)?;
    schema_objects(&reference)
}

fn schema_is_adoptable_v0(connection: &Connection) -> Result<bool, SqlError> {
    let actual = schema_objects(connection)?;
    Ok(actual.is_empty() || actual == expected_schema_objects()?)
}

fn schema_is_current(connection: &Connection) -> Result<bool, SqlError> {
    Ok(schema_objects(connection)? == expected_schema_objects()?)
}

fn classify_connection(connection: &Connection) -> Result<StoreCompatibility, SqlError> {
    let version =
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    match version {
        0 if schema_is_adoptable_v0(connection)? => Ok(StoreCompatibility::Current),
        schema::CURRENT_VERSION if schema_is_current(connection)? => {
            Ok(StoreCompatibility::Current)
        }
        _ => Ok(StoreCompatibility::Unsupported),
    }
}

fn validate_foreign_keys(connection: &Connection) -> Result<(), SqlError> {
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    if statement.exists([])? {
        return Err(schema_error("thought store foreign-key validation failed"));
    }
    Ok(())
}

fn schema_error(message: &str) -> SqlError {
    SqlError::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_SCHEMA),
        Some(message.to_string()),
    )
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Store, SqlError> {
        Store::wrap(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store, SqlError> {
        Store::wrap(Connection::open_in_memory()?)
    }

    fn wrap(mut conn: Connection) -> Result<Store, SqlError> {
        let compatibility = classify_connection(&conn)?;
        if compatibility == StoreCompatibility::Unsupported {
            return Err(schema_error("unsupported thought store schema version"));
        }
        let version = conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
        if version == 0 {
            conn.pragma_update(None, "foreign_keys", true)?;
            let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let locked_version =
                transaction.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
            if locked_version != 0 || !schema_is_adoptable_v0(&transaction)? {
                return Err(schema_error("thought store changed during schema adoption"));
            }
            transaction.execute_batch(schema::SCHEMA)?;
            if !schema_is_current(&transaction)? {
                return Err(schema_error("thought store schema adoption failed"));
            }
            validate_foreign_keys(&transaction)?;
            transaction.pragma_update(None, "user_version", schema::CURRENT_VERSION)?;
            transaction.commit()?;
        }

        // WAL so a reader never blocks the writer. NORMAL trades a fsync per
        // commit for the small risk of losing the last few updates on power
        // loss — acceptable because the relay and the peer replicas hold them
        // too, and unacceptable latency is worse for a writing app.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Store { conn })
    }

    pub fn upsert_actor(&self, actor: &Actor) -> Result<(), SqlError> {
        self.conn.execute(
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
        transaction.commit()
    }

    pub fn create_initial_document_with_lineage(
        &self,
        document: InitialDocument<'_>,
        expected_seq: i64,
        event: ProvenanceEventInput<'_>,
        spans: &[LineageSpanRow],
    ) -> Result<(), SqlError> {
        let transaction = self.conn.unchecked_transaction()?;
        let timestamp = now_ms();
        transaction.execute(
            "INSERT INTO documents (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![document.id, document.title, timestamp],
        )?;
        transaction.execute(
            "INSERT INTO updates
               (seq, doc_id, payload, actor_id, origin, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                expected_seq,
                document.id,
                document.payload,
                document.actor_id,
                document.origin.as_str(),
                document.session_id,
                timestamp,
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
                    document.attributed_at,
                ],
            )?;
        }
        insert_event(&transaction, document.id, expected_seq, &event)?;
        replace_lineage_spans(&transaction, document.id, spans)?;
        transaction.commit()
    }

    pub fn next_update_seq(&self) -> Result<i64, SqlError> {
        self.conn
            .query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM updates", [], |row| {
                row.get(0)
            })
    }

    pub fn commit_lineage_update(&self, update: LineageUpdate<'_>) -> Result<i64, SqlError> {
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO updates
               (seq, doc_id, payload, actor_id, origin, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                update.expected_seq,
                update.doc_id,
                update.payload,
                update.actor_id,
                update.origin.as_str(),
                update.session_id,
                update.event.created_at,
            ],
        )?;
        transaction.execute(
            "UPDATE documents
             SET title = ?2, updated_at = ?3, deleted_at = ?4
             WHERE id = ?1",
            params![
                update.doc_id,
                update.title,
                update.event.created_at,
                update.deleted_at,
            ],
        )?;
        transaction.execute("DELETE FROM doc_fts WHERE doc_id = ?1", [update.doc_id])?;
        transaction.execute(
            "INSERT INTO doc_fts (doc_id, title, body) VALUES (?1, ?2, ?3)",
            params![update.doc_id, update.title, update.markdown],
        )?;
        for block_id in update.touched_blocks {
            transaction.execute(
                "INSERT INTO block_provenance
                   (doc_id, block_id, created_by, created_at, touched_by, touched_at, session_id)
                 VALUES (?1, ?2, ?3, ?5, ?3, ?5, ?4)
                 ON CONFLICT(doc_id, block_id) DO UPDATE SET
                   touched_by = excluded.touched_by,
                   touched_at = excluded.touched_at,
                   session_id = excluded.session_id",
                params![
                    update.doc_id,
                    block_id,
                    update.actor_id,
                    update.session_id,
                    update.event.created_at,
                ],
            )?;
        }
        let current = update
            .current_blocks
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let mut statement =
            transaction.prepare("SELECT block_id FROM block_provenance WHERE doc_id = ?1")?;
        let present = statement
            .query_map([update.doc_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for block_id in present.iter().filter(|id| !current.contains(*id)) {
            transaction.execute(
                "DELETE FROM block_provenance WHERE doc_id = ?1 AND block_id = ?2",
                params![update.doc_id, block_id],
            )?;
        }
        insert_event(
            &transaction,
            update.doc_id,
            update.expected_seq,
            &update.event,
        )?;
        replace_lineage_spans(&transaction, update.doc_id, update.spans)?;
        transaction.commit()?;
        Ok(update.expected_seq)
    }

    pub fn seed_legacy_lineage(
        &self,
        doc_id: &str,
        update_seq: i64,
        event: ProvenanceEventInput<'_>,
        spans: &[LineageSpanRow],
    ) -> Result<(), SqlError> {
        let transaction = self.conn.unchecked_transaction()?;
        insert_event(&transaction, doc_id, update_seq, &event)?;
        replace_lineage_spans(&transaction, doc_id, spans)?;
        transaction.commit()
    }

    pub fn latest_update_seq(&self, doc_id: &str) -> Result<Option<i64>, SqlError> {
        self.conn.query_row(
            "SELECT MAX(seq) FROM updates WHERE doc_id = ?1",
            [doc_id],
            |row| row.get(0),
        )
    }

    pub fn provenance_events(&self, doc_id: &str) -> Result<Vec<ProvenanceEventRow>, SqlError> {
        let mut statement = self.conn.prepare(
            "SELECT event_id, actor_id, action, group_key, source_label, ingress,
                    assurance, alignment, session_id, created_at
             FROM provenance_events WHERE doc_id = ?1 ORDER BY event_id",
        )?;
        statement
            .query_map([doc_id], |row| {
                Ok(ProvenanceEventRow {
                    event_id: row.get(0)?,
                    actor_id: row.get(1)?,
                    action: row.get(2)?,
                    group_key: row.get(3)?,
                    source_label: row.get(4)?,
                    ingress: row.get(5)?,
                    assurance: row.get(6)?,
                    alignment: row.get(7)?,
                    session_id: row.get(8)?,
                    created_at: row.get(9)?,
                })
            })?
            .collect()
    }

    pub fn lineage_spans(&self, doc_id: &str) -> Result<Vec<LineageSpanRow>, SqlError> {
        let mut statement = self.conn.prepare(
            "SELECT block_id, node_path, start_utf16, end_utf16, source_event_id
             FROM lineage_spans WHERE doc_id = ?1
             ORDER BY block_id, node_path, start_utf16",
        )?;
        statement
            .query_map([doc_id], |row| {
                Ok(LineageSpanRow {
                    block_id: row.get(0)?,
                    node_path: row.get(1)?,
                    start_utf16: row.get(2)?,
                    end_utf16: row.get(3)?,
                    source_event_id: row.get(4)?,
                })
            })?
            .collect()
    }

    /// Append one update frame. Callers batch an agent turn into a single frame
    /// before calling (AD-16); this is one INSERT, not a transaction per op.
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

    /// Persist CRDT metadata without manufacturing a text-lineage event.
    ///
    /// Suggestions live in the document CRDT, but proposing or rejecting one
    /// does not change document wording. Keep the update and document timestamp
    /// atomic while leaving FTS, block provenance, and lineage untouched.
    pub fn commit_metadata_update(
        &self,
        doc_id: &str,
        payload: &[u8],
        actor_id: &str,
        origin: Origin,
        session_id: Option<&str>,
    ) -> Result<i64, SqlError> {
        let transaction = self.conn.unchecked_transaction()?;
        let timestamp = now_ms();
        transaction.execute(
            "INSERT INTO updates (doc_id, payload, actor_id, origin, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                doc_id,
                payload,
                actor_id,
                origin.as_str(),
                session_id,
                timestamp
            ],
        )?;
        let sequence = transaction.last_insert_rowid();
        transaction.execute(
            "UPDATE documents SET updated_at = ?2 WHERE id = ?1",
            params![doc_id, timestamp],
        )?;
        transaction.commit()?;
        Ok(sequence)
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
        let mut stmt = self.conn.prepare(
            "SELECT seq, payload, actor_id, origin, session_id, created_at
             FROM updates WHERE doc_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(params![doc_id], |row| {
                Ok(LoggedUpdate {
                    seq: row.get(0)?,
                    payload: row.get(1)?,
                    actor_id: row.get(2)?,
                    origin: Origin::parse(&row.get::<_, String>(3)?),
                    session_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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
        self.conn.query_row(
            "SELECT COUNT(*) FROM updates WHERE doc_id = ?1 AND seq > ?2",
            params![doc_id, through],
            |row| row.get(0),
        )
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
        let mut stmt = self.conn.prepare(
            "SELECT doc_id, snippet(doc_fts, 2, '<b>', '</b>', '…', 12)
             FROM doc_fts WHERE doc_fts MATCH ?1 LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![query, limit as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn search_document(
        &self,
        query: &str,
        document_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>, SqlError> {
        let mut statement = self.conn.prepare(
            "SELECT doc_id, snippet(doc_fts, 2, '<b>', '</b>', '…', 12)
             FROM doc_fts WHERE doc_fts MATCH ?1 AND doc_id = ?2 LIMIT ?3",
        )?;
        statement
            .query_map(params![query, document_id, limit as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect()
    }

    /// Documents, most recently updated first.
    ///
    /// `trashed` selects which side of the tombstone to look at. Deleting is
    /// soft (AD-14), so the rows never leave — but without this there was no way
    /// back to a document once deleted, which makes "soft" a claim rather than a
    /// feature.
    pub fn list_documents(&self, trashed: bool) -> Result<Vec<DocumentRow>, SqlError> {
        let sql = if trashed {
            "SELECT id, title, updated_at, deleted_at FROM documents
             WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC"
        } else {
            "SELECT id, title, updated_at, deleted_at FROM documents
             WHERE deleted_at IS NULL ORDER BY updated_at DESC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(DocumentRow {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    updated_at: row.get(2)?,
                    deleted: row.get::<_, Option<i64>>(3)?.is_some(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn create_reviewer_connection(
        &self,
        input: &NewReviewerConnection<'_>,
    ) -> Result<ReviewerConnectionRow, SqlError> {
        self.conn.execute(
            "INSERT INTO reviewer_connections (
               id, client, display_label, document_scope, document_id,
               credential_hash, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                input.id,
                input.client,
                input.display_label,
                input.document_scope,
                input.document_id,
                input.credential_hash,
                input.created_at,
            ],
        )?;
        self.reviewer_connection(input.id)?
            .ok_or(SqlError::QueryReturnedNoRows)
    }

    pub fn reviewer_connection(&self, id: &str) -> Result<Option<ReviewerConnectionRow>, SqlError> {
        self.conn
            .query_row(
                "SELECT id, client, display_label, document_scope, document_id,
                        credential_hash, revision, created_at, updated_at,
                        last_seen_at, revoked_at, reported_model
                 FROM reviewer_connections WHERE id = ?1",
                [id],
                reviewer_connection_row,
            )
            .optional()
    }

    pub fn list_reviewer_connections(&self) -> Result<Vec<ReviewerConnectionRow>, SqlError> {
        let mut statement = self.conn.prepare(
            "SELECT id, client, display_label, document_scope, document_id,
                    credential_hash, revision, created_at, updated_at,
                    last_seen_at, revoked_at, reported_model
             FROM reviewer_connections
             ORDER BY revoked_at IS NOT NULL, updated_at DESC",
        )?;
        statement
            .query_map([], reviewer_connection_row)?
            .collect::<Result<Vec<_>, _>>()
    }

    pub fn reviewer_connection_by_credential_hash(
        &self,
        credential_hash: &[u8; 32],
    ) -> Result<Option<ReviewerConnectionRow>, SqlError> {
        self.conn
            .query_row(
                "SELECT id, client, display_label, document_scope, document_id,
                        credential_hash, revision, created_at, updated_at,
                        last_seen_at, revoked_at, reported_model
                 FROM reviewer_connections
                 WHERE credential_hash = ?1 AND revoked_at IS NULL",
                [credential_hash],
                reviewer_connection_row,
            )
            .optional()
    }

    pub fn update_reviewer_connection(
        &self,
        id: &str,
        expected_revision: i64,
        display_label: &str,
        document_scope: &str,
        document_id: Option<&str>,
        updated_at: i64,
    ) -> Result<Option<ReviewerConnectionRow>, SqlError> {
        let changed = self.conn.execute(
            "UPDATE reviewer_connections
             SET display_label = ?3, document_scope = ?4, document_id = ?5,
                 revision = revision + 1, updated_at = ?6
             WHERE id = ?1 AND revision = ?2 AND revoked_at IS NULL",
            params![
                id,
                expected_revision,
                display_label,
                document_scope,
                document_id,
                updated_at,
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.reviewer_connection(id)
    }

    pub fn rotate_reviewer_credential(
        &self,
        id: &str,
        expected_revision: i64,
        credential_hash: &[u8; 32],
        updated_at: i64,
    ) -> Result<Option<ReviewerConnectionRow>, SqlError> {
        let changed = self.conn.execute(
            "UPDATE reviewer_connections
             SET credential_hash = ?3, revision = revision + 1, updated_at = ?4,
                 last_seen_at = NULL, reported_model = NULL
             WHERE id = ?1 AND revision = ?2 AND revoked_at IS NULL",
            params![id, expected_revision, credential_hash, updated_at],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.reviewer_connection(id)
    }

    pub fn revoke_reviewer_connection(
        &self,
        id: &str,
        expected_revision: i64,
        revoked_at: i64,
    ) -> Result<Option<ReviewerConnectionRow>, SqlError> {
        let changed = self.conn.execute(
            "UPDATE reviewer_connections
             SET revoked_at = ?3, revision = revision + 1, updated_at = ?3
             WHERE id = ?1 AND revision = ?2 AND revoked_at IS NULL",
            params![id, expected_revision, revoked_at],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.reviewer_connection(id)
    }

    pub fn mark_reviewer_seen(
        &self,
        id: &str,
        seen_at: i64,
    ) -> Result<Option<ReviewerConnectionRow>, SqlError> {
        let changed = self.conn.execute(
            "UPDATE reviewer_connections
             SET last_seen_at = MAX(COALESCE(last_seen_at, 0), ?2)
             WHERE id = ?1 AND revoked_at IS NULL",
            params![id, seen_at],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.reviewer_connection(id)
    }

    pub fn update_reviewer_reported_model(
        &self,
        id: &str,
        model: Option<&str>,
    ) -> Result<Option<ReviewerConnectionRow>, SqlError> {
        let changed = self.conn.execute(
            "UPDATE reviewer_connections SET reported_model = ?2
             WHERE id = ?1 AND revoked_at IS NULL",
            params![id, model],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.reviewer_connection(id)
    }

    /// Everyone who has written to a document, most recent first.
    pub fn actors_for_document(&self, doc_id: &str) -> Result<Vec<ActorActivity>, SqlError> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.kind, a.display_name, a.model, a.color,
                    MAX(u.created_at) AS last_seen, COUNT(*) AS edits
             FROM updates u JOIN actors a ON a.id = u.actor_id
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
    pub fn provenance_for_document(&self, doc_id: &str) -> Result<Vec<BlockAttribution>, SqlError> {
        let mut stmt = self.conn.prepare(
            "SELECT p.block_id, p.created_by, p.created_at, p.touched_by, p.touched_at,
                    p.session_id, a.kind, a.display_name, a.model, a.color
             FROM block_provenance p JOIN actors a ON a.id = p.touched_by
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

fn insert_event(
    transaction: &Transaction<'_>,
    doc_id: &str,
    update_seq: i64,
    event: &ProvenanceEventInput<'_>,
) -> Result<(), SqlError> {
    transaction.execute(
        "INSERT INTO provenance_events (
           event_id, doc_id, update_seq, actor_id, action, group_key, source_label,
           ingress, assurance, alignment, session_id, created_at
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
         )",
        params![
            event.event_id,
            doc_id,
            update_seq,
            event.actor_id,
            event.action,
            event.group_key,
            event.source_label,
            event.ingress,
            event.assurance,
            event.alignment,
            event.session_id,
            event.created_at,
        ],
    )?;
    Ok(())
}

fn reviewer_connection_row(row: &rusqlite::Row<'_>) -> Result<ReviewerConnectionRow, SqlError> {
    let hash = row.get::<_, Vec<u8>>(5)?;
    let credential_hash: [u8; 32] = hash.try_into().map_err(|value: Vec<u8>| {
        SqlError::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Blob,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "reviewer credential hash must be 32 bytes",
            )
            .into(),
        )
    })?;
    Ok(ReviewerConnectionRow {
        id: row.get(0)?,
        client: row.get(1)?,
        display_label: row.get(2)?,
        document_scope: row.get(3)?,
        document_id: row.get(4)?,
        credential_hash,
        revision: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        last_seen_at: row.get(9)?,
        revoked_at: row.get(10)?,
        reported_model: row.get(11)?,
    })
}

fn replace_lineage_spans(
    transaction: &Transaction<'_>,
    doc_id: &str,
    spans: &[LineageSpanRow],
) -> Result<(), SqlError> {
    transaction.execute("DELETE FROM lineage_spans WHERE doc_id = ?1", [doc_id])?;
    for span in spans {
        transaction.execute(
            "INSERT INTO lineage_spans (
               doc_id, block_id, node_path, start_utf16, end_utf16, source_event_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                doc_id,
                span.block_id,
                span.node_path,
                span.start_utf16,
                span.end_utf16,
                span.source_event_id,
            ],
        )?;
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
