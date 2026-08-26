//! Transactional persistence for semantic provenance. These tests use opaque
//! bytes and strings intentionally: hashing and lineage computation belong to
//! the caller, while this crate guarantees storage order and atomicity.

use rusqlite::Connection;
use thought_store::{
    Actor, BlockTouchInput, InitialProvenanceDocumentInput, LineageRebuildInput, LineageSpanInput,
    Origin, ProvenanceAnchorInput, ProvenanceChangeInput, ProvenanceCommitInput,
    ProvenanceEventInput, ProvenanceRecordInput, ProvenanceUpdateInput, ReadyLineageInput, Store,
    StoreError,
};

fn seed_actor(store: &Store) {
    store.upsert_actor(&actor()).unwrap();
}

fn actor() -> Actor {
    Actor {
        id: "human:test".into(),
        kind: "human".into(),
        display_name: "Test Writer".into(),
        model: None,
        color: "#123456".into(),
    }
}

fn event(event_id: i64, ingress: &str, client_event_id: Option<&str>) -> ProvenanceEventInput {
    let (assurance, source_label) = match ingress {
        "unknown" => ("unknown", "Unclassified change"),
        "legacy_unknown" => ("unknown", "Legacy content"),
        "mcp" => ("reported", "Test Writer (reported)"),
        "api" => ("verified", "Test Provider (verified)"),
        "suggestion" => ("reported", "Test Writer (reported)"),
        "pasted" => ("observed", "Pasted"),
        "imported" => ("observed", "Imported"),
        "command" => ("observed", "Edited here"),
        _ => ("observed", "Written here"),
    };
    ProvenanceEventInput {
        event_id,
        actor_id: Some("human:test".into()),
        action: "edit".into(),
        ingress: ingress.into(),
        assurance: assurance.into(),
        connection_id: None,
        session_id: Some(format!("session-{event_id}")),
        actor_label: "Test Writer".into(),
        source_label: source_label.into(),
        provider: None,
        requested_model: None,
        reported_model: None,
        evidence_ref: None,
        suggestion_id: None,
        client_event_id: client_event_id.map(str::to_string),
        chain_version: 1,
        before_hash: vec![event_id as u8; 32],
        after_hash: vec![(event_id as u8).wrapping_add(1); 32],
        update_log_root: vec![(event_id as u8).wrapping_add(2); 32],
        previous_event_hash: (event_id > 1).then(|| vec![(event_id - 1) as u8; 32]),
        event_hash: vec![(event_id as u8).wrapping_add(3); 32],
        created_at: 100 + event_id,
        recorded_at: 200 + event_id,
    }
}

fn update(id: i64) -> ProvenanceUpdateInput {
    ProvenanceUpdateInput {
        expected_seq: id,
        payload: vec![id as u8],
        actor_id: "human:test".into(),
        origin: Origin::Human,
        session_id: Some(format!("session-{id}")),
        created_at: 200 + id,
    }
}

fn insert_change(source_event_id: i64, block_id: &str, text: &str) -> ProvenanceChangeInput {
    ProvenanceChangeInput {
        op: "insert".into(),
        source_event_id: Some(source_event_id),
        before_block_id: None,
        before_path: None,
        before_from_utf16: None,
        before_to_utf16: None,
        after_block_id: Some(block_id.into()),
        after_path: Some("[0]".into()),
        after_from_utf16: Some(0),
        after_to_utf16: Some(text.encode_utf16().count() as i64),
        before_text: String::new(),
        after_text: text.into(),
        before_format: None,
        after_format: Some(String::new()),
        before_shape: None,
        after_shape: None,
    }
}

fn anchor(
    basis: &str,
    before_start: i64,
    before_end: i64,
    after_start: i64,
    after_end: i64,
    digest: u8,
) -> ProvenanceAnchorInput {
    ProvenanceAnchorInput {
        basis: basis.into(),
        before_start_grapheme: before_start,
        before_end_grapheme: before_end,
        after_start_grapheme: after_start,
        after_end_grapheme: after_end,
        before_text_hash: vec![digest; 32],
        after_text_hash: vec![digest.wrapping_add(1); 32],
    }
}

fn span(source_event_id: i64, block_id: &str, start: i64, end: i64) -> LineageSpanInput {
    LineageSpanInput {
        block_id: block_id.into(),
        node_path: "[0]".into(),
        start_utf16: start,
        end_utf16: end,
        source_event_id,
    }
}

fn lineage(version: i64, digest: u8) -> ReadyLineageInput {
    ReadyLineageInput {
        algorithm_version: version,
        lineage_digest: vec![digest],
        rebuilt_at: 300 + version,
    }
}

fn initial(doc_id: &str, event_id: i64, with_text: bool) -> InitialProvenanceDocumentInput {
    let text = if with_text { "hello" } else { "" };
    InitialProvenanceDocumentInput {
        id: doc_id.into(),
        title: "Original".into(),
        markdown: text.into(),
        created_at: 1,
        updated_at: 1,
        actor: actor(),
        update: update(event_id),
        event: event(event_id, "entered", Some(&format!("{doc_id}-create"))),
        changes: with_text
            .then(|| insert_change(event_id, "block-old", text))
            .into_iter()
            .collect(),
        anchors: vec![],
        spans: with_text
            .then(|| span(event_id, "block-old", 0, 5))
            .into_iter()
            .collect(),
        lineage: lineage(1, event_id as u8),
        block_ids: vec!["block-old".into()],
        attributed_at: 1,
    }
}

fn revision(doc_id: &str, event_id: i64, ingress: &str) -> ProvenanceCommitInput {
    ProvenanceCommitInput {
        doc_id: doc_id.into(),
        title: format!("Revision {event_id}"),
        markdown: format!("hello revision {event_id}"),
        updated_at: 400 + event_id,
        deleted_at: None,
        actor: actor(),
        update: update(event_id),
        event: event(
            event_id,
            ingress,
            Some(&format!("{doc_id}-event-{event_id}")),
        ),
        changes: vec![insert_change(
            event_id,
            "block-current",
            &format!("revision {event_id}"),
        )],
        anchors: vec![],
        spans: vec![span(event_id, "block-current", 0, 10)],
        lineage: lineage(1, event_id as u8),
        block_touches: vec![BlockTouchInput {
            block_id: "block-current".into(),
            actor_id: "human:test".into(),
            session_id: Some(format!("session-{event_id}")),
            at: 400 + event_id,
        }],
        current_block_ids: vec!["block-current".into()],
    }
}

#[test]
fn initial_document_and_revision_round_trip_as_complete_transactions() {
    let store = Store::open_in_memory().unwrap();
    seed_actor(&store);
    assert_eq!(store.next_provenance_event_id().unwrap(), 1);
    assert_eq!(store.next_update_seq().unwrap(), 1);

    let created = store
        .create_initial_document_with_provenance(&initial("doc", 1, true))
        .unwrap();
    assert_eq!(created.update_seq, Some(1));
    assert_eq!(created.event_id, 1);
    assert!(!created.replayed);

    let committed = store
        .commit_update_with_provenance(&revision("doc", 2, "command"))
        .unwrap();
    assert_eq!(committed.update_seq, Some(2));
    assert_eq!(committed.event_id, 2);
    assert_eq!(store.next_provenance_event_id().unwrap(), 3);
    assert_eq!(store.next_update_seq().unwrap(), 3);

    let events = store.provenance_events("doc").unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].ingress, "entered");
    assert_eq!(events[1].ingress, "command");
    assert_eq!(events[1].update_seq, Some(2));

    let changes = store.provenance_changes(2).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].ordinal, 0);
    assert_eq!(changes[0].change.source_event_id, Some(2));
    assert_eq!(changes[0].change.after_text, "revision 2");

    let spans = store.lineage_spans("doc").unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].span, span(2, "block-current", 0, 10));
    let state = store.lineage_state("doc").unwrap().unwrap();
    assert_eq!(state.through_update_seq, 2);
    assert_eq!(state.through_event_id, 2);
    assert_eq!(state.state, "ready");

    let updates = store.updates_for_rebuild("doc").unwrap();
    assert_eq!(
        updates.iter().map(|update| update.seq).collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(store.search("revision", 10).unwrap()[0].0, "doc");
    assert_eq!(store.list_documents(false).unwrap()[0].title, "Revision 2");

    let compatibility = store.provenance_for_document("doc").unwrap();
    assert_eq!(compatibility.len(), 1);
    assert_eq!(compatibility[0].block_id, "block-current");
    assert_eq!(compatibility[0].touched_by, "human:test");
}

#[test]
fn mixed_chain_versions_round_trip_with_event_local_anchor_order() {
    let store = Store::open_in_memory().unwrap();
    store
        .create_initial_document_with_provenance(&initial("doc", 1, true))
        .unwrap();

    let mut revision = revision("doc", 2, "command");
    revision.event.chain_version = 2;
    revision.anchors = vec![
        anchor("editor_transaction", 1, 2, 1, 3, 10),
        anchor("server_operation", 4, 5, 5, 5, 20),
    ];
    store.commit_update_with_provenance(&revision).unwrap();

    let events = store.provenance_events("doc").unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.chain_version)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(store.provenance_anchors(1).unwrap().is_empty());
    let anchors = store.provenance_anchors(2).unwrap();
    assert_eq!(
        anchors
            .iter()
            .map(|anchor| anchor.ordinal)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(anchors[0].anchor, revision.anchors[0]);
    assert_eq!(anchors[1].anchor, revision.anchors[1]);
}

#[test]
fn chain_version_anchor_contract_is_validated_before_writes() {
    let store = Store::open_in_memory().unwrap();
    store
        .create_initial_document_with_provenance(&initial("doc", 1, true))
        .unwrap();

    let mut v1_with_anchor = revision("doc", 2, "command");
    v1_with_anchor.anchors = vec![anchor("editor_transaction", 0, 1, 0, 1, 1)];
    assert!(matches!(
        store.commit_update_with_provenance(&v1_with_anchor),
        Err(StoreError::InvalidProvenanceAnchors { event_id: 2, .. })
    ));

    let mut v2_without_anchor = revision("doc", 2, "command");
    v2_without_anchor.event.chain_version = 2;
    assert!(matches!(
        store.commit_update_with_provenance(&v2_without_anchor),
        Err(StoreError::InvalidProvenanceAnchors { event_id: 2, .. })
    ));

    let mut unsupported = revision("doc", 2, "command");
    unsupported.event.chain_version = 3;
    assert!(matches!(
        store.commit_update_with_provenance(&unsupported),
        Err(StoreError::InvalidProvenanceAnchors { event_id: 2, .. })
    ));

    assert_eq!(store.provenance_events("doc").unwrap().len(), 1);
    assert_eq!(store.updates_for_rebuild("doc").unwrap().len(), 1);
}

#[test]
fn initial_and_provenance_only_writes_share_the_anchor_contract() {
    let store = Store::open_in_memory().unwrap();

    let mut missing_initial_anchor = initial("missing", 1, true);
    missing_initial_anchor.event.chain_version = 2;
    assert!(matches!(
        store.create_initial_document_with_provenance(&missing_initial_anchor),
        Err(StoreError::InvalidProvenanceAnchors { event_id: 1, .. })
    ));
    assert!(store.list_documents(true).unwrap().is_empty());

    let mut anchored_initial = initial("doc", 1, true);
    anchored_initial.event.chain_version = 2;
    anchored_initial.anchors = vec![anchor("editor_transaction", 0, 0, 0, 5, 1)];
    store
        .create_initial_document_with_provenance(&anchored_initial)
        .unwrap();

    let mut missing_record_anchor = ProvenanceRecordInput {
        doc_id: "doc".into(),
        event: event(2, "unknown", None),
        changes: vec![],
        anchors: vec![],
        spans: vec![span(1, "block-old", 0, 5)],
        lineage: lineage(2, 2),
        bind_to_latest_update: false,
    };
    missing_record_anchor.event.chain_version = 2;
    assert!(matches!(
        store.record_provenance_without_update(&missing_record_anchor),
        Err(StoreError::InvalidProvenanceAnchors { event_id: 2, .. })
    ));

    missing_record_anchor.anchors = vec![anchor("server_operation", 0, 0, 0, 0, 2)];
    store
        .record_provenance_without_update(&missing_record_anchor)
        .unwrap();
    assert_eq!(store.provenance_anchors(1).unwrap().len(), 1);
    assert_eq!(store.provenance_anchors(2).unwrap().len(), 1);
}

#[test]
fn invalid_anchor_ranges_order_hashes_and_basis_roll_back_cleanly() {
    let store = Store::open_in_memory().unwrap();
    store
        .create_initial_document_with_provenance(&initial("doc", 1, true))
        .unwrap();

    let mut invalid_inputs = Vec::new();

    let mut negative = revision("doc", 2, "command");
    negative.event.chain_version = 2;
    negative.anchors = vec![anchor("editor_transaction", -1, 1, 0, 1, 1)];
    invalid_inputs.push(negative);

    let mut reversed = revision("doc", 2, "command");
    reversed.event.chain_version = 2;
    reversed.anchors = vec![anchor("editor_transaction", 2, 1, 0, 1, 1)];
    invalid_inputs.push(reversed);

    let mut overlapping = revision("doc", 2, "command");
    overlapping.event.chain_version = 2;
    overlapping.anchors = vec![
        anchor("editor_transaction", 0, 3, 0, 1, 1),
        anchor("editor_transaction", 2, 4, 1, 2, 2),
    ];
    invalid_inputs.push(overlapping);

    let mut bad_hash = revision("doc", 2, "command");
    bad_hash.event.chain_version = 2;
    let mut short_hash = anchor("editor_transaction", 0, 1, 0, 1, 1);
    short_hash.after_text_hash.pop();
    bad_hash.anchors = vec![short_hash];
    invalid_inputs.push(bad_hash);

    let mut bad_basis = revision("doc", 2, "command");
    bad_basis.event.chain_version = 2;
    bad_basis.anchors = vec![anchor("inferred", 0, 1, 0, 1, 1)];
    invalid_inputs.push(bad_basis);

    for input in invalid_inputs {
        assert!(matches!(
            store.commit_update_with_provenance(&input),
            Err(StoreError::InvalidProvenanceAnchors { event_id: 2, .. })
        ));
    }

    assert_eq!(store.provenance_events("doc").unwrap().len(), 1);
    assert_eq!(store.updates_for_rebuild("doc").unwrap().len(), 1);
    assert!(store.provenance_anchors(2).unwrap().is_empty());
}

#[test]
fn latest_provenance_event_reads_only_the_requested_document_tail() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.latest_provenance_event("missing").unwrap().is_none());

    store
        .create_initial_document_with_provenance(&initial("one", 1, true))
        .unwrap();
    store
        .commit_update_with_provenance(&revision("one", 2, "command"))
        .unwrap();
    store
        .create_initial_document_with_provenance(&initial("two", 3, true))
        .unwrap();

    let latest = store.latest_provenance_event("one").unwrap().unwrap();
    assert_eq!(latest.event_id, 2);
    assert_eq!(latest.doc_id, "one");
    assert_eq!(latest.ingress, "command");
}

#[test]
fn evidence_reads_raw_origin_while_typed_activity_rejects_unknown_values() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    {
        let store = Store::open(&path).unwrap();
        store
            .create_initial_document_with_provenance(&initial("doc", 1, true))
            .unwrap();
    }

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP TRIGGER updates_reject_evidence_update;")
        .unwrap();
    connection
        .execute(
            "UPDATE updates SET origin = 'bogus' WHERE doc_id = 'doc'",
            [],
        )
        .unwrap();
    drop(connection);

    let store = Store::open(&path).unwrap();
    assert_eq!(store.updates_for_rebuild("doc").unwrap()[0].origin, "bogus");
    assert!(matches!(
        store.log("doc"),
        Err(StoreError::InvalidStoredOrigin { seq: 1, value }) if value == "bogus"
    ));
}

#[test]
fn provenance_change_sources_accept_prior_events_from_the_same_document() {
    let store = Store::open_in_memory().unwrap();
    store
        .create_initial_document_with_provenance(&initial("doc", 1, true))
        .unwrap();
    let mut commit = revision("doc", 2, "command");
    commit.changes[0].source_event_id = Some(1);

    store.commit_update_with_provenance(&commit).unwrap();

    assert_eq!(
        store.provenance_changes(2).unwrap()[0]
            .change
            .source_event_id,
        Some(1)
    );
}

#[test]
fn provenance_change_sources_reject_events_from_another_document() {
    let store = Store::open_in_memory().unwrap();
    store
        .create_initial_document_with_provenance(&initial("one", 1, true))
        .unwrap();
    store
        .create_initial_document_with_provenance(&initial("two", 2, true))
        .unwrap();
    let mut commit = revision("one", 3, "command");
    commit.changes[0].source_event_id = Some(2);

    assert!(matches!(
        store.commit_update_with_provenance(&commit),
        Err(StoreError::ProvenanceChangeSourceMismatch {
            doc_id,
            event_id: 3,
            source_event_id: 2,
        }) if doc_id == "one"
    ));
    assert!(store.provenance_changes(3).unwrap().is_empty());
    assert_eq!(store.updates_for_rebuild("one").unwrap().len(), 1);
}

#[test]
fn provenance_change_sources_reject_later_events_from_the_same_document() {
    let store = Store::open_in_memory().unwrap();
    store
        .create_initial_document_with_provenance(&initial("doc", 1, true))
        .unwrap();
    let mut later = revision("doc", 3, "command");
    later.update.expected_seq = 2;
    store.commit_update_with_provenance(&later).unwrap();

    let attempted = ProvenanceRecordInput {
        doc_id: "doc".into(),
        event: event(2, "unknown", None),
        changes: vec![insert_change(3, "block-current", "future")],
        anchors: vec![],
        spans: vec![span(1, "block-current", 0, 5)],
        lineage: lineage(1, 2),
        bind_to_latest_update: false,
    };
    assert!(matches!(
        store.record_provenance_without_update(&attempted),
        Err(StoreError::ProvenanceChangeSourceInFuture {
            event_id: 2,
            source_event_id: 3,
        })
    ));
    assert!(store.provenance_changes(2).unwrap().is_empty());
    assert_eq!(
        store
            .provenance_events("doc")
            .unwrap()
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        [1, 3]
    );
}

#[test]
fn failed_provenance_commit_rolls_back_actor_metadata_changes() {
    let store = Store::open_in_memory().unwrap();
    store
        .create_initial_document_with_provenance(&initial("doc", 1, true))
        .unwrap();
    let original = &store.actors_for_document("doc").unwrap()[0];
    assert_eq!(original.display_name, "Test Writer");
    assert_eq!(original.model, None);
    assert_eq!(original.color, "#123456");

    let mut failed = revision("doc", 2, "command");
    failed.actor.display_name = "Uncommitted Name".into();
    failed.actor.model = Some("uncommitted-model".into());
    failed.actor.color = "#ffffff".into();
    failed.spans = vec![span(999, "block-current", 0, 1)];
    assert!(matches!(
        store.commit_update_with_provenance(&failed),
        Err(StoreError::LineageSourceMismatch {
            source_event_id: 999,
            ..
        })
    ));

    let preserved = &store.actors_for_document("doc").unwrap()[0];
    assert_eq!(preserved.display_name, "Test Writer");
    assert_eq!(preserved.model, None);
    assert_eq!(preserved.color, "#123456");
}

#[test]
fn a_mid_transaction_lineage_failure_rolls_back_every_authoritative_row() {
    let store = Store::open_in_memory().unwrap();
    seed_actor(&store);
    store
        .create_initial_document_with_provenance(&initial("doc", 1, true))
        .unwrap();
    let before_spans = store.lineage_spans("doc").unwrap();

    let mut failed = revision("doc", 2, "unknown");
    failed.event.chain_version = 2;
    failed.anchors = vec![anchor("editor_transaction", 0, 1, 0, 2, 40)];
    // The update, event, anchors, and change are inserted before lineage validation.
    // This invalid source forces a failure in the middle of the transaction.
    failed.spans = vec![span(999, "block-current", 0, 1)];
    failed.deleted_at = Some(999);
    assert!(matches!(
        store.commit_update_with_provenance(&failed),
        Err(StoreError::LineageSourceMismatch {
            source_event_id: 999,
            ..
        })
    ));

    assert_eq!(store.updates_for_rebuild("doc").unwrap().len(), 1);
    assert_eq!(store.provenance_events("doc").unwrap().len(), 1);
    assert!(store.provenance_anchors(2).unwrap().is_empty());
    assert!(store.provenance_changes(2).unwrap().is_empty());
    assert_eq!(store.lineage_spans("doc").unwrap(), before_spans);
    assert_eq!(
        store
            .lineage_state("doc")
            .unwrap()
            .unwrap()
            .through_event_id,
        1
    );
    let document = &store.list_documents(false).unwrap()[0];
    assert_eq!(document.title, "Original");
    assert!(!document.deleted);
    assert!(store.search("revision", 10).unwrap().is_empty());
    assert_eq!(store.next_provenance_event_id().unwrap(), 2);
    assert_eq!(store.next_update_seq().unwrap(), 2);
}

#[test]
fn client_event_retries_are_idempotent_and_conflicts_never_append() {
    let store = Store::open_in_memory().unwrap();
    seed_actor(&store);
    store
        .create_initial_document_with_provenance(&initial("doc", 1, true))
        .unwrap();
    let commit = revision("doc", 2, "command");

    let first = store.commit_update_with_provenance(&commit).unwrap();
    let retry = store.commit_update_with_provenance(&commit).unwrap();
    assert!(!first.replayed);
    assert!(retry.replayed);
    assert_eq!(retry.update_seq, first.update_seq);
    assert_eq!(store.updates_for_rebuild("doc").unwrap().len(), 2);
    assert_eq!(store.provenance_events("doc").unwrap().len(), 2);

    let mut conflict = commit.clone();
    conflict.event.event_id = 3;
    assert!(matches!(
        store.commit_update_with_provenance(&conflict),
        Err(StoreError::IdempotencyConflict { .. })
    ));
    assert_eq!(store.updates_for_rebuild("doc").unwrap().len(), 2);
    assert_eq!(store.provenance_events("doc").unwrap().len(), 2);

    // With no client key, the explicit primary-key insert is the final
    // collision check. Its preceding update is rolled back too.
    let mut event_id_collision = revision("doc", 2, "unknown");
    event_id_collision.update.expected_seq = 3;
    event_id_collision.event.client_event_id = None;
    event_id_collision.event.event_hash = vec![99];
    assert!(event_id_collision.event.event_id > 0);
    assert!(
        store
            .commit_update_with_provenance(&event_id_collision)
            .is_err()
    );
    assert_eq!(store.updates_for_rebuild("doc").unwrap().len(), 2);
    assert_eq!(store.provenance_events("doc").unwrap().len(), 2);
}

#[test]
fn deletion_projection_commits_with_the_update_and_provenance() {
    let store = Store::open_in_memory().unwrap();
    seed_actor(&store);
    store
        .create_initial_document_with_provenance(&initial("doc", 1, true))
        .unwrap();
    let mut deleted = revision("doc", 2, "command");
    deleted.deleted_at = Some(777);

    store.commit_update_with_provenance(&deleted).unwrap();

    let documents = store.list_documents(true).unwrap();
    assert_eq!(documents.len(), 1);
    assert!(documents[0].deleted);
    assert_eq!(store.provenance_events("doc").unwrap().len(), 2);
    assert_eq!(store.updates_for_rebuild("doc").unwrap().len(), 2);
}

#[test]
fn legacy_seed_binds_to_the_existing_update_and_empty_spans_are_ready() {
    let store = Store::open_in_memory().unwrap();
    seed_actor(&store);
    store.create_document("legacy", "Legacy").unwrap();
    let update_seq = store
        .append_update(
            "legacy",
            &[7],
            "human:test",
            Origin::Human,
            Some("old-session"),
        )
        .unwrap();
    let input = ProvenanceRecordInput {
        doc_id: "legacy".into(),
        event: event(1, "legacy_unknown", Some("legacy-seed")),
        changes: vec![],
        anchors: vec![],
        spans: vec![],
        lineage: lineage(1, 42),
        bind_to_latest_update: true,
    };

    let recorded = store.record_provenance_without_update(&input).unwrap();
    assert_eq!(recorded.update_seq, Some(update_seq));
    assert_eq!(store.updates_for_rebuild("legacy").unwrap().len(), 1);
    assert_eq!(
        store.provenance_events("legacy").unwrap()[0].update_seq,
        Some(update_seq)
    );
    assert!(store.lineage_spans("legacy").unwrap().is_empty());
    let state = store.lineage_state("legacy").unwrap().unwrap();
    assert_eq!(state.state, "ready");
    assert_eq!(state.through_update_seq, update_seq);
    assert_eq!(state.through_event_id, 1);
}

#[test]
fn command_and_unknown_are_distinct_current_ingress_values() {
    let store = Store::open_in_memory().unwrap();
    seed_actor(&store);
    store
        .create_initial_document_with_provenance(&initial("doc", 1, false))
        .unwrap();
    store
        .commit_update_with_provenance(&revision("doc", 2, "command"))
        .unwrap();
    store
        .commit_update_with_provenance(&revision("doc", 3, "unknown"))
        .unwrap();

    let ingress = store
        .provenance_events("doc")
        .unwrap()
        .into_iter()
        .map(|event| event.ingress)
        .collect::<Vec<_>>();
    assert_eq!(ingress, ["entered", "command", "unknown"]);
}

#[test]
fn cache_rebuild_changes_no_evidence_and_rejects_cross_document_sources() {
    let store = Store::open_in_memory().unwrap();
    seed_actor(&store);
    store
        .create_initial_document_with_provenance(&initial("one", 1, true))
        .unwrap();
    store
        .create_initial_document_with_provenance(&initial("two", 2, true))
        .unwrap();
    let updates_before = store.updates_for_rebuild("one").unwrap();
    let events_before = store.provenance_events("one").unwrap();

    store
        .rebuild_lineage_cache(&LineageRebuildInput {
            doc_id: "one".into(),
            spans: vec![span(1, "rebuilt", 0, 3)],
            lineage: lineage(2, 88),
            through_update_seq: 1,
            through_event_id: 1,
        })
        .unwrap();
    assert_eq!(store.updates_for_rebuild("one").unwrap(), updates_before);
    assert_eq!(store.provenance_events("one").unwrap(), events_before);
    assert_eq!(
        store.lineage_spans("one").unwrap()[0].span.block_id,
        "rebuilt"
    );
    assert_eq!(
        store
            .lineage_state("one")
            .unwrap()
            .unwrap()
            .algorithm_version,
        2
    );

    let before_failed_rebuild = store.lineage_spans("one").unwrap();
    assert!(matches!(
        store.rebuild_lineage_cache(&LineageRebuildInput {
            doc_id: "one".into(),
            spans: vec![span(2, "wrong-document", 0, 1)],
            lineage: lineage(3, 99),
            through_update_seq: 1,
            through_event_id: 1,
        }),
        Err(StoreError::LineageSourceMismatch {
            source_event_id: 2,
            ..
        })
    ));
    assert_eq!(store.lineage_spans("one").unwrap(), before_failed_rebuild);
    assert_eq!(
        store
            .lineage_state("one")
            .unwrap()
            .unwrap()
            .algorithm_version,
        2
    );
}
