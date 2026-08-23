//! M1.0 acceptance criterion 2: the op log attributes every change to the right
//! actor and survives a daemon restart.

use polar_core::Document;
use polar_schema::{Node, normalize};
use polar_store::{Actor, Origin, Store};

fn actor(id: &str, kind: &str) -> Actor {
    Actor {
        id: id.into(),
        kind: kind.into(),
        display_name: id.into(),
        model: None,
        color: "#4c8dff".into(),
    }
}

fn seed(store: &Store, doc_id: &str) {
    store.upsert_actor(&actor("human:kev", "human")).unwrap();
    store.upsert_actor(&actor("agent:opus", "agent")).unwrap();
    store.create_document(doc_id, "Test").unwrap();
}

/// Restore a document from whatever the store holds, the way a cold start does.
fn restore(store: &Store, doc_id: &str) -> Document {
    let restored = store.restore(doc_id).unwrap();
    let doc = Document::new();
    if let Some(state) = &restored.snapshot {
        doc.apply_update(state).unwrap();
    }
    for update in &restored.updates {
        doc.apply_update(update).unwrap();
    }
    doc
}

#[test]
fn a_thousand_updates_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("polar.db");
    let doc_id = "doc-1";

    let live = Document::new();
    let expected_sv;

    {
        let store = Store::open(&path).unwrap();
        seed(&store, doc_id);

        live.set_document(&normalize(&Node::element(
            "doc",
            vec![Node::element(
                "paragraph",
                vec![Node::text("start", vec![])],
            )],
        )));
        store
            .append_update(
                doc_id,
                &live.encode_state(),
                "human:kev",
                Origin::Human,
                None,
            )
            .unwrap();

        let block = live.blocks()[0].block_id.clone();
        for i in 0..1000 {
            let before = live.state_vector();
            live.append_text(&block, &format!(" {i}")).unwrap();
            // Only the delta is logged, which is what an update frame is.
            let delta = live.diff_since(&before);
            store
                .append_update(doc_id, &delta, "agent:opus", Origin::Agent, Some("run-7"))
                .unwrap();
        }
        expected_sv = live.state_vector();
    } // store dropped: the daemon has exited

    let store = Store::open(&path).unwrap();
    let reloaded = restore(&store, doc_id);

    assert_eq!(
        reloaded.state_vector(),
        expected_sv,
        "state diverged across restart"
    );
    assert_eq!(normalize(&reloaded.read()), normalize(&live.read()));

    let log = store.log(doc_id).unwrap();
    assert_eq!(log.len(), 1001);
    assert_eq!(log[0].origin, Origin::Human);
    assert!(log[1..].iter().all(|u| u.origin == Origin::Agent));
    assert!(
        log[1..]
            .iter()
            .all(|u| u.session_id.as_deref() == Some("run-7"))
    );
    // Sequence numbers must be strictly increasing, or "replay in order" is a
    // lie and per-run revert cannot reconstruct anything.
    assert!(log.windows(2).all(|w| w[0].seq < w[1].seq));
}

/// AD-13 draws a line most storage layers do not: snapshots serve load
/// performance and are discardable, the op log serves provenance and is not.
/// Compaction must never cross it.
#[test]
fn snapshotting_never_truncates_the_op_log() {
    let store = Store::open_in_memory().unwrap();
    let doc_id = "doc-2";
    seed(&store, doc_id);

    let doc = Document::new();
    doc.set_document(&normalize(&Node::element(
        "doc",
        vec![Node::element("paragraph", vec![Node::text("x", vec![])])],
    )));
    let mut last_seq = store
        .append_update(
            doc_id,
            &doc.encode_state(),
            "human:kev",
            Origin::Human,
            None,
        )
        .unwrap();

    let block = doc.blocks()[0].block_id.clone();
    for i in 0..50 {
        let before = doc.state_vector();
        doc.append_text(&block, &format!("{i}")).unwrap();
        last_seq = store
            .append_update(
                doc_id,
                &doc.diff_since(&before),
                "agent:opus",
                Origin::Agent,
                None,
            )
            .unwrap();
    }

    assert_eq!(store.updates_since_snapshot(doc_id).unwrap(), 51);

    store
        .write_snapshot(doc_id, last_seq, &doc.encode_state(), &doc.state_vector())
        .unwrap();

    assert_eq!(
        store.log(doc_id).unwrap().len(),
        51,
        "compaction truncated the provenance log"
    );
    assert_eq!(store.updates_since_snapshot(doc_id).unwrap(), 0);

    // And restoring now costs one snapshot instead of 51 replays.
    let restored = store.restore(doc_id).unwrap();
    assert!(restored.snapshot.is_some());
    assert!(restored.updates.is_empty());
    assert_eq!(restore(&store, doc_id).state_vector(), doc.state_vector());
}

#[test]
fn restore_replays_updates_recorded_after_a_snapshot() {
    let store = Store::open_in_memory().unwrap();
    let doc_id = "doc-3";
    seed(&store, doc_id);

    let doc = Document::new();
    doc.set_document(&normalize(&Node::element(
        "doc",
        vec![Node::element("paragraph", vec![Node::text("a", vec![])])],
    )));
    let seq = store
        .append_update(
            doc_id,
            &doc.encode_state(),
            "human:kev",
            Origin::Human,
            None,
        )
        .unwrap();
    store
        .write_snapshot(doc_id, seq, &doc.encode_state(), &doc.state_vector())
        .unwrap();

    let block = doc.blocks()[0].block_id.clone();
    let before = doc.state_vector();
    doc.append_text(&block, "-after").unwrap();
    store
        .append_update(
            doc_id,
            &doc.diff_since(&before),
            "agent:opus",
            Origin::Agent,
            None,
        )
        .unwrap();

    let restored = restore(&store, doc_id);
    assert_eq!(restored.block_text(&block).unwrap(), "a-after");
    assert_eq!(restored.state_vector(), doc.state_vector());
}

#[test]
fn search_finds_documents_by_body() {
    let store = Store::open_in_memory().unwrap();
    seed(&store, "doc-4");
    store.create_document("doc-5", "Other").unwrap();

    store
        .reindex("doc-4", "Meeting notes", "the quarterly roadmap review")
        .unwrap();
    store
        .reindex("doc-5", "Other", "unrelated content")
        .unwrap();

    let hits = store.search("roadmap", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "doc-4");

    // reindex refreshes the denormalized title used by list views.
    let listed = store.list_documents().unwrap();
    assert!(
        listed
            .iter()
            .any(|d| d.id == "doc-4" && d.title == "Meeting notes")
    );
}
