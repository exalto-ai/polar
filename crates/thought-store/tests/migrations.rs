//! Database upgrades are a compatibility boundary, not an implementation
//! detail. This fixture freezes the schema released before migrations existed
//! and proves that adopting it preserves user data.

use rusqlite::{Connection, params};
use thought_store::{SCHEMA_VERSION, Store, StoreError};

const LEGACY_V0_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS documents (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  deleted_at INTEGER,
  share_id TEXT,
  relay_url TEXT
);
CREATE TABLE IF NOT EXISTS updates (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  doc_id TEXT NOT NULL REFERENCES documents(id),
  payload BLOB NOT NULL,
  actor_id TEXT NOT NULL REFERENCES actors(id),
  origin TEXT NOT NULL,
  session_id TEXT,
  created_at INTEGER NOT NULL,
  synced_at INTEGER
);
CREATE INDEX IF NOT EXISTS updates_doc_seq ON updates(doc_id, seq);
CREATE INDEX IF NOT EXISTS updates_unsynced ON updates(doc_id) WHERE synced_at IS NULL;
CREATE TABLE IF NOT EXISTS snapshots (
  doc_id TEXT NOT NULL REFERENCES documents(id),
  through_seq INTEGER NOT NULL,
  state BLOB NOT NULL,
  state_vector BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (doc_id, through_seq)
);
CREATE TABLE IF NOT EXISTS actors (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  display_name TEXT NOT NULL,
  model TEXT,
  color TEXT NOT NULL,
  first_seen INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS block_provenance (
  doc_id TEXT NOT NULL REFERENCES documents(id),
  block_id TEXT NOT NULL,
  created_by TEXT NOT NULL REFERENCES actors(id),
  created_at INTEGER NOT NULL,
  touched_by TEXT NOT NULL REFERENCES actors(id),
  touched_at INTEGER NOT NULL,
  session_id TEXT,
  PRIMARY KEY (doc_id, block_id)
);
CREATE INDEX IF NOT EXISTS block_provenance_doc ON block_provenance(doc_id);
CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts USING fts5(
  doc_id UNINDEXED, title, body, tokenize='porter unicode61'
);
"#;

fn user_version(connection: &Connection) -> i64 {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn object_exists(connection: &Connection, kind: &str, name: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_schema WHERE type = ?1 AND name = ?2
             )",
            params![kind, name],
            |row| row.get(0),
        )
        .unwrap()
}

fn create_legacy_database(path: &std::path::Path, with_data: bool) {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(LEGACY_V0_SCHEMA).unwrap();
    assert_eq!(user_version(&connection), 0);

    if !with_data {
        return;
    }
    connection
        .execute(
            "INSERT INTO actors (id, kind, display_name, model, color, first_seen)
             VALUES ('human:legacy', 'human', 'Legacy Writer', NULL, '#123456', 10)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO documents
                 (id, title, created_at, updated_at, deleted_at, share_id, relay_url)
             VALUES ('legacy-doc', 'A preserved draft', 11, 12, NULL, 'share', 'relay')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO updates
                 (doc_id, payload, actor_id, origin, session_id, created_at, synced_at)
             VALUES ('legacy-doc', X'010203', 'human:legacy', 'human', 'old-session', 12, 13)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO snapshots
                 (doc_id, through_seq, state, state_vector, created_at)
             VALUES ('legacy-doc', 1, X'0405', X'06', 14)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO block_provenance
                 (doc_id, block_id, created_by, created_at, touched_by, touched_at, session_id)
             VALUES (
                 'legacy-doc', '1:0', 'human:legacy', 11,
                 'human:legacy', 12, 'old-session'
             )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO doc_fts (doc_id, title, body)
             VALUES ('legacy-doc', 'A preserved draft', 'legacy words survive')",
            [],
        )
        .unwrap();
}

type LegacyDocument = (
    String,
    String,
    i64,
    i64,
    Option<i64>,
    Option<String>,
    Option<String>,
);
type LegacyUpdate = (
    i64,
    String,
    Vec<u8>,
    String,
    String,
    Option<String>,
    i64,
    Option<i64>,
);
type LegacySnapshot = (String, i64, Vec<u8>, Vec<u8>, i64);
type LegacyActor = (String, String, String, Option<String>, String, i64);
type LegacyBlock = (String, String, String, i64, String, i64, Option<String>);
type LegacySearch = (String, String, String);

#[derive(Debug, PartialEq, Eq)]
struct LegacyData {
    document: LegacyDocument,
    update: LegacyUpdate,
    snapshot: LegacySnapshot,
    actor: LegacyActor,
    block: LegacyBlock,
    search: LegacySearch,
}

fn read_legacy_data(connection: &Connection) -> LegacyData {
    LegacyData {
        document: connection
            .query_row(
                "SELECT id, title, created_at, updated_at, deleted_at, share_id, relay_url
                 FROM documents",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap(),
        update: connection
            .query_row(
                "SELECT seq, doc_id, payload, actor_id, origin, session_id, created_at, synced_at
                 FROM updates",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .unwrap(),
        snapshot: connection
            .query_row(
                "SELECT doc_id, through_seq, state, state_vector, created_at FROM snapshots",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap(),
        actor: connection
            .query_row(
                "SELECT id, kind, display_name, model, color, first_seen FROM actors",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap(),
        block: connection
            .query_row(
                "SELECT doc_id, block_id, created_by, created_at,
                        touched_by, touched_at, session_id
                 FROM block_provenance",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap(),
        search: connection
            .query_row("SELECT doc_id, title, body FROM doc_fts", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap(),
    }
}

#[test]
fn a_fresh_database_reaches_the_current_version() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");

    drop(Store::open(&path).unwrap());

    let connection = Connection::open(&path).unwrap();
    assert_eq!(user_version(&connection), SCHEMA_VERSION);
    for table in [
        "documents",
        "updates",
        "provenance_events",
        "provenance_changes",
        "provenance_anchors",
        "provenance_receipts",
        "lineage_spans",
        "lineage_state",
        "reviewer_connections",
        "reviewer_connection_documents",
        "reviewer_connection_events",
    ] {
        assert!(
            object_exists(&connection, "table", table),
            "missing {table}"
        );
    }
    let violations: i64 = connection
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn the_unversioned_released_schema_is_adopted_without_data_loss() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    create_legacy_database(&path, true);
    let before = {
        let connection = Connection::open(&path).unwrap();
        read_legacy_data(&connection)
    };

    drop(Store::open(&path).unwrap());

    let connection = Connection::open(&path).unwrap();
    assert_eq!(user_version(&connection), SCHEMA_VERSION);
    assert_eq!(read_legacy_data(&connection), before);
    let document: (String, i64, String, String) = connection
        .query_row(
            "SELECT title, updated_at, share_id, relay_url
             FROM documents WHERE id = 'legacy-doc'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        document,
        (
            "A preserved draft".into(),
            12,
            "share".into(),
            "relay".into()
        )
    );
    let update: (Vec<u8>, String, Option<String>, Option<i64>) = connection
        .query_row(
            "SELECT payload, actor_id, session_id, synced_at FROM updates WHERE seq = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        update,
        (
            vec![1, 2, 3],
            "human:legacy".into(),
            Some("old-session".into()),
            Some(13)
        )
    );
    let snapshot: (Vec<u8>, Vec<u8>) = connection
        .query_row(
            "SELECT state, state_vector FROM snapshots
             WHERE doc_id = 'legacy-doc' AND through_seq = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(snapshot, (vec![4, 5], vec![6]));
    let attributed: String = connection
        .query_row(
            "SELECT touched_by FROM block_provenance
             WHERE doc_id = 'legacy-doc' AND block_id = '1:0'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(attributed, "human:legacy");
    let indexed: String = connection
        .query_row(
            "SELECT body FROM doc_fts WHERE doc_id = 'legacy-doc'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexed, "legacy words survive");
}

#[test]
fn reopening_the_current_schema_is_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");

    drop(Store::open(&path).unwrap());
    let before = {
        let connection = Connection::open(&path).unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type IN ('table', 'index', 'trigger')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    };

    drop(Store::open(&path).unwrap());

    let connection = Connection::open(&path).unwrap();
    let after: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type IN ('table', 'index', 'trigger')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(user_version(&connection), SCHEMA_VERSION);
}

#[test]
fn a_future_schema_version_is_refused_without_modification() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    {
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("CREATE TABLE future_data (value TEXT NOT NULL)", [])
            .unwrap();
        connection
            .execute("INSERT INTO future_data VALUES ('keep me')", [])
            .unwrap();
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
    }

    match Store::open(&path) {
        Err(StoreError::FutureSchemaVersion { found, supported }) => {
            assert_eq!(found, SCHEMA_VERSION + 1);
            assert_eq!(supported, SCHEMA_VERSION);
        }
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("an older app opened a newer database"),
    }

    let connection = Connection::open(&path).unwrap();
    assert_eq!(user_version(&connection), SCHEMA_VERSION + 1);
    let value: String = connection
        .query_row("SELECT value FROM future_data", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "keep me");
    assert!(!object_exists(&connection, "table", "documents"));
}

#[test]
fn a_failed_migration_rolls_back_its_schema_and_version() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    create_legacy_database(&path, false);
    {
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        // V2 creates provenance_events before this table. The collision forces
        // a failure after work has begun and proves the whole V2 transaction
        // rolls back.
        connection
            .execute("CREATE TABLE provenance_changes (sentinel TEXT)", [])
            .unwrap();
    }

    match Store::open(&path) {
        Err(StoreError::MigrationFailed { version: 2, .. }) => {}
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("a conflicting schema was silently adopted"),
    }

    let connection = Connection::open(&path).unwrap();
    assert_eq!(user_version(&connection), 1);
    assert!(!object_exists(&connection, "table", "provenance_events"));
    assert!(!object_exists(
        &connection,
        "index",
        "updates_doc_seq_unique"
    ));
    let columns: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('provenance_changes')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 1, "the pre-existing collision remains untouched");
}

#[test]
fn provenance_ledger_rows_reject_update_and_delete() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    drop(Store::open(&path).unwrap());

    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    connection
        .execute(
            "INSERT INTO actors (id, kind, display_name, color, first_seen)
             VALUES ('human:test', 'human', 'Test Writer', '#123456', 1)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO documents (id, title, created_at, updated_at)
             VALUES ('doc', 'Draft', 1, 1)",
            [],
        )
        .unwrap();
    assert!(
        connection
            .execute(
                "INSERT INTO updates (doc_id, payload, actor_id, origin, created_at)
                 VALUES ('doc', X'00', 'human:test', 'bogus', 1)",
                [],
            )
            .is_err(),
        "new update rows must use the closed origin vocabulary"
    );
    connection
        .execute(
            "INSERT INTO updates (doc_id, payload, actor_id, origin, created_at)
             VALUES ('doc', X'01', 'human:test', 'human', 1)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO provenance_events (
                 doc_id, update_seq, action, ingress, assurance, actor_id,
                 actor_label, source_label, chain_version, before_hash, after_hash,
                 update_log_root, event_hash, created_at, recorded_at
             ) VALUES (
                 'doc', 1, 'edit', 'entered', 'observed',
                 'human:test', 'Test Writer', 'Written here', 2, zeroblob(32), zeroblob(32),
                 zeroblob(32), X'0303030303030303030303030303030303030303030303030303030303030303',
                 1, 1
             )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO provenance_anchors (
                 event_id, ordinal, basis,
                 before_start_grapheme, before_end_grapheme,
                 after_start_grapheme, after_end_grapheme,
                 before_text_hash, after_text_hash
             ) VALUES (
                 1, 0, 'editor_transaction', 0, 1, 0, 1,
                 zeroblob(32), zeroblob(32)
             )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO provenance_changes (
                 event_id, ordinal, op, source_event_id, after_block_id,
                 after_path, after_from_utf16, after_to_utf16, after_text
             ) VALUES (1, 0, 'insert', 1, '1:0', '[]', 0, 1, 'x')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO provenance_receipts (
                 receipt_id, event_id, receipt_kind, issuer,
                 artifact_digest, payload_format, payload, created_at
             ) VALUES (
                 'receipt', 1, 'device_signature', 'this-device',
                 X'04', 'application/octet-stream', X'05', 1
             )",
            [],
        )
        .unwrap();

    for statement in [
        "UPDATE updates SET payload = X'02' WHERE seq = 1",
        "DELETE FROM updates WHERE seq = 1",
        "UPDATE provenance_events SET actor_label = 'changed' WHERE event_id = 1",
        "DELETE FROM provenance_events WHERE event_id = 1",
        "UPDATE provenance_changes SET after_text = 'changed' WHERE event_id = 1",
        "DELETE FROM provenance_changes WHERE event_id = 1",
        "UPDATE provenance_anchors SET basis = 'server_operation' WHERE event_id = 1",
        "DELETE FROM provenance_anchors WHERE event_id = 1",
        "UPDATE provenance_receipts SET issuer = 'changed' WHERE seq = 1",
        "DELETE FROM provenance_receipts WHERE seq = 1",
    ] {
        assert!(
            connection.execute(statement, []).is_err(),
            "append-only guard accepted: {statement}"
        );
    }

    connection
        .execute("UPDATE updates SET synced_at = 2 WHERE seq = 1", [])
        .expect("relay acknowledgement remains mutable operational state");
}

#[test]
fn anchor_schema_rejects_invalid_foreign_keys_ranges_order_hashes_and_basis() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    drop(Store::open(&path).unwrap());

    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    connection
        .execute(
            "INSERT INTO actors (id, kind, display_name, color, first_seen)
             VALUES ('human:test', 'human', 'Test Writer', '#123456', 1)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO documents (id, title, created_at, updated_at)
             VALUES ('doc', 'Draft', 1, 1)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO provenance_events (
                 event_id, doc_id, action, ingress, assurance, actor_id,
                 actor_label, source_label, chain_version, before_hash, after_hash,
                 update_log_root, event_hash, created_at, recorded_at
             ) VALUES (
                 1, 'doc', 'edit', 'entered', 'observed', 'human:test',
                 'Test Writer', 'Written here', 2, zeroblob(32), zeroblob(32),
                 zeroblob(32), X'0101010101010101010101010101010101010101010101010101010101010101',
                 1, 1
             )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO provenance_events (
                 event_id, doc_id, action, ingress, assurance, actor_id,
                 actor_label, source_label, chain_version, before_hash, after_hash,
                 update_log_root, event_hash, created_at, recorded_at
             ) VALUES (
                 2, 'doc', 'edit', 'entered', 'observed', 'human:test',
                 'Test Writer', 'Written here', 3, zeroblob(32), zeroblob(32),
                 zeroblob(32), X'0202020202020202020202020202020202020202020202020202020202020202',
                 2, 2
             )",
            [],
        )
        .expect_err("the schema admits only event-chain versions 1 and 2");

    let anchor_values = |event_id: i64,
                         ordinal: i64,
                         basis: &str,
                         before_start: i64,
                         before_end: i64,
                         after_start: i64,
                         after_end: i64,
                         before_hash: Vec<u8>,
                         after_hash: Vec<u8>| {
        connection.execute(
            "INSERT INTO provenance_anchors (
                 event_id, ordinal, basis,
                 before_start_grapheme, before_end_grapheme,
                 after_start_grapheme, after_end_grapheme,
                 before_text_hash, after_text_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                event_id,
                ordinal,
                basis,
                before_start,
                before_end,
                after_start,
                after_end,
                before_hash,
                after_hash
            ],
        )
    };

    for result in [
        anchor_values(
            999,
            0,
            "editor_transaction",
            0,
            1,
            0,
            1,
            vec![0; 32],
            vec![0; 32],
        ),
        anchor_values(1, 0, "inferred", 0, 1, 0, 1, vec![0; 32], vec![0; 32]),
        anchor_values(
            1,
            0,
            "editor_transaction",
            -1,
            1,
            0,
            1,
            vec![0; 32],
            vec![0; 32],
        ),
        anchor_values(
            1,
            0,
            "editor_transaction",
            2,
            1,
            0,
            1,
            vec![0; 32],
            vec![0; 32],
        ),
        anchor_values(
            1,
            0,
            "editor_transaction",
            0,
            1,
            0,
            1,
            vec![0; 31],
            vec![0; 32],
        ),
        anchor_values(
            1,
            2,
            "editor_transaction",
            0,
            1,
            0,
            1,
            vec![0; 32],
            vec![0; 32],
        ),
    ] {
        assert!(result.is_err());
    }

    anchor_values(
        1,
        0,
        "server_operation",
        0,
        0,
        0,
        2,
        vec![1; 32],
        vec![2; 32],
    )
    .unwrap();
    anchor_values(
        1,
        2,
        "server_operation",
        2,
        2,
        4,
        4,
        vec![3; 32],
        vec![4; 32],
    )
    .expect_err("event-local anchor ordinals cannot skip a value");

    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM provenance_anchors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}
