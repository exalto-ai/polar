//! Durable storage: an append-only op log, periodic snapshots, and the actors
//! that wrote them (M1.4).
//!
//! Deliberately ignorant of CRDT semantics — it stores opaque update frames.
//! Two retention policies live here and they are not the same policy:
//! snapshots exist for *load performance* and may be discarded freely, while
//! the op log exists for *provenance* and is never compacted away, because the
//! activity feed and per-run revert read it (AD-13).

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::HashSet;
use std::path::Path;

mod schema;

pub use rusqlite::Error as SqlError;

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

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Store, SqlError> {
        Store::wrap(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store, SqlError> {
        Store::wrap(Connection::open_in_memory()?)
    }

    fn wrap(conn: Connection) -> Result<Store, SqlError> {
        // WAL so a reader never blocks the writer. NORMAL trades a fsync per
        // commit for the small risk of losing the last few updates on power
        // loss — acceptable because the relay and the peer replicas hold them
        // too, and unacceptable latency is worse for a writing app.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), SqlError> {
        self.conn.execute_batch(schema::SCHEMA)
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
