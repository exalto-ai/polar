use thought_mcp::{ActorRef, TextEdit, Workspace};
use thought_provenance::{Alignment, Assurance, Ingress};

#[test]
fn a_small_edit_only_claims_the_text_it_added() {
    let workspace = Workspace::open_in_memory().unwrap();
    let human = ActorRef::human("writer");
    let agent = ActorRef::agent("reviewer", Some("test-model"), Some("review-1"));
    let created = workspace
        .create_document_from_markdown("", "The quick brown fox.", &human)
        .unwrap();

    workspace
        .replace_text(
            &created.doc_id,
            &created.blocks[0].block_id,
            &TextEdit {
                find: "brown",
                replace: "vivid",
                occurrence: Some(1),
            },
            None,
            &agent,
        )
        .unwrap();

    let lineage = workspace.document_lineage(&created.doc_id).unwrap();
    assert_eq!(lineage.summary.contributions.len(), 2);
    let agent_text = lineage
        .summary
        .contributions
        .iter()
        .find(|item| item.source.label == "reviewer")
        .unwrap();
    assert_eq!(agent_text.non_whitespace_graphemes, 5);
    assert_eq!(agent_text.source.ingress, Ingress::Mcp);
    assert_eq!(agent_text.source.assurance, Assurance::Reported);
    assert_eq!(agent_text.source.alignment, Alignment::Inferred);
}

#[test]
fn current_lineage_survives_restart_without_replaying_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let actor = ActorRef::human("writer");
    let (doc_id, before) = {
        let workspace = Workspace::open(&path).unwrap();
        let created = workspace
            .create_document_from_markdown("", "Durable text.", &actor)
            .unwrap();
        let lineage = workspace.document_lineage(&created.doc_id).unwrap();
        (created.doc_id, serde_json::to_value(lineage).unwrap())
    };

    let workspace = Workspace::open(&path).unwrap();
    let after = serde_json::to_value(workspace.document_lineage(&doc_id).unwrap()).unwrap();
    assert_eq!(after, before);
}

#[test]
fn documents_without_lineage_are_labeled_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let doc_id = {
        let workspace = Workspace::open(&path).unwrap();
        workspace
            .create_document("Legacy", &ActorRef::human("writer"))
            .unwrap()
            .doc_id
    };
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute("DELETE FROM lineage_spans", []).unwrap();
    connection
        .execute("DELETE FROM provenance_events", [])
        .unwrap();
    drop(connection);

    let workspace = Workspace::open(&path).unwrap();
    let lineage = workspace.document_lineage(&doc_id).unwrap();
    assert_eq!(lineage.summary.contributions.len(), 1);
    let source = &lineage.summary.contributions[0].source;
    assert_eq!(source.ingress, Ingress::LegacyUnknown);
    assert_eq!(source.assurance, Assurance::Unknown);
    assert_eq!(source.alignment, Alignment::Unknown);
}
