use rusqlite::{Connection, OpenFlags};
use std::fs;
use std::path::Path;
use thought_store::{Store, StoreCompatibility, inspect_compatibility};

const CURRENT_VERSION: i64 = 7;

fn user_version(path: &Path) -> i64 {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn create_exact_current_v0(path: &Path) {
    drop(Store::open(path).unwrap());
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "INSERT INTO actors VALUES (
               'human:test', 'human', 'Writer', NULL, '#123456', 10
             );
             INSERT INTO documents VALUES (
               'doc-test', 'Preserved draft', 10, 11, NULL, NULL, NULL
             );
             INSERT INTO updates (
               seq, doc_id, payload, actor_id, origin, session_id, created_at
             ) VALUES (
               7, 'doc-test', X'00FF80', 'human:test', 'human', 'session-test', 12
             );
             PRAGMA user_version = 0;",
        )
        .unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

fn create_rejected_store(path: &Path, version: i64) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode = DELETE;
             CREATE TABLE preview_data (id INTEGER PRIMARY KEY, payload BLOB NOT NULL);
             INSERT INTO preview_data VALUES (1, X'00FF80');",
        )
        .unwrap();
    connection
        .pragma_update(None, "user_version", version)
        .unwrap();
}

fn assert_rejected_without_mutation(path: &Path, version: i64) {
    let before = fs::read(path).unwrap();
    assert_eq!(
        inspect_compatibility(path).unwrap(),
        StoreCompatibility::Unsupported
    );
    assert!(Store::open(path).is_err());
    assert_eq!(fs::read(path).unwrap(), before);

    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        version
    );
    assert_eq!(
        connection
            .query_row("SELECT payload FROM preview_data WHERE id = 1", [], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .unwrap(),
        vec![0, 255, 128]
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'documents'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn accepted_v0_adopts_version_7_and_preserves_existing_data() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    create_exact_current_v0(&path);
    assert_eq!(
        inspect_compatibility(&path).unwrap(),
        StoreCompatibility::Current
    );

    drop(Store::open(&path).unwrap());

    let connection = Connection::open(&path).unwrap();
    assert_eq!(user_version(&path), CURRENT_VERSION);
    assert_eq!(
        connection
            .query_row(
                "SELECT title FROM documents WHERE id = 'doc-test'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "Preserved draft"
    );
    assert_eq!(
        connection
            .query_row("SELECT payload FROM updates WHERE seq = 7", [], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .unwrap(),
        vec![0, 255, 128]
    );
}

#[test]
fn exact_current_version_7_reopens_idempotently() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let store = Store::open(&path).unwrap();
    store
        .create_document("doc-current", "Current draft")
        .unwrap();
    drop(store);
    assert_eq!(user_version(&path), CURRENT_VERSION);
    assert_eq!(
        inspect_compatibility(&path).unwrap(),
        StoreCompatibility::Current
    );

    drop(Store::open(&path).unwrap());

    let connection = Connection::open(&path).unwrap();
    assert_eq!(user_version(&path), CURRENT_VERSION);
    assert_eq!(
        connection
            .query_row(
                "SELECT title FROM documents WHERE id = 'doc-current'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "Current draft"
    );
}

#[test]
fn preview_and_future_versions_fail_closed_without_mutation() {
    for version in [6, CURRENT_VERSION + 1] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("thought-v{version}.db"));
        create_rejected_store(&path, version);
        assert_rejected_without_mutation(&path, version);
    }
}

#[test]
fn conflicting_version_0_table_fails_closed_without_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode = DELETE;
             CREATE TABLE documents (id INTEGER PRIMARY KEY, payload BLOB NOT NULL);
             INSERT INTO documents VALUES (1, X'00FF80');",
        )
        .unwrap();
    drop(connection);
    let before = fs::read(&path).unwrap();

    assert_eq!(
        inspect_compatibility(&path).unwrap(),
        StoreCompatibility::Unsupported
    );
    assert!(Store::open(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);

    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(user_version(&path), 0);
    assert_eq!(
        connection
            .query_row("SELECT payload FROM documents WHERE id = 1", [], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .unwrap(),
        vec![0, 255, 128]
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'updates'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}
