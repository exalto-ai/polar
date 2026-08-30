use thought_store::{
    Actor, InitialDocument, LineageSpanRow, LineageUpdate, Origin, ProvenanceEventInput, Store,
};

fn event(id: i64, at: i64) -> ProvenanceEventInput<'static> {
    ProvenanceEventInput {
        event_id: id,
        actor_id: Some("human:writer"),
        action: "edit",
        group_key: "local:written",
        source_label: "Written here",
        ingress: "entered",
        assurance: "observed",
        alignment: "inferred",
        session_id: None,
        created_at: at,
    }
}

#[test]
fn an_update_and_its_current_lineage_commit_together() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let store = Store::open(&path).unwrap();
    store
        .upsert_actor(&Actor {
            id: "human:writer".into(),
            kind: "human".into(),
            display_name: "Writer".into(),
            model: None,
            color: "#000".into(),
        })
        .unwrap();
    let initial_spans = vec![LineageSpanRow {
        block_id: "block-1".into(),
        node_path: "[0]".into(),
        start_utf16: 0,
        end_utf16: 4,
        source_event_id: 1,
    }];
    store
        .create_initial_document_with_lineage(
            InitialDocument {
                id: "doc-1",
                title: "Before",
                payload: b"first",
                actor_id: "human:writer",
                origin: Origin::Human,
                session_id: None,
                markdown: "text",
                block_ids: &["block-1".into()],
                attributed_at: 1,
            },
            1,
            event(1, 1),
            &initial_spans,
        )
        .unwrap();

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_test_lineage
             BEFORE INSERT ON lineage_spans
             WHEN NEW.start_utf16 = 999
             BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
        )
        .unwrap();
    drop(connection);

    let bad_spans = vec![LineageSpanRow {
        block_id: "block-1".into(),
        node_path: "[0]".into(),
        start_utf16: 999,
        end_utf16: 1000,
        source_event_id: 2,
    }];
    assert!(
        store
            .commit_lineage_update(LineageUpdate {
                doc_id: "doc-1",
                expected_seq: 2,
                payload: b"second",
                actor_id: "human:writer",
                origin: Origin::Human,
                session_id: None,
                title: "After",
                markdown: "changed",
                deleted_at: None,
                touched_blocks: &["block-1".into()],
                current_blocks: &["block-1".into()],
                event: event(2, 2),
                spans: &bad_spans,
            })
            .is_err()
    );

    assert_eq!(store.log("doc-1").unwrap().len(), 1);
    assert_eq!(store.provenance_events("doc-1").unwrap().len(), 1);
    assert_eq!(store.lineage_spans("doc-1").unwrap(), initial_spans);
    assert_eq!(store.list_documents(false).unwrap()[0].title, "Before");
}
