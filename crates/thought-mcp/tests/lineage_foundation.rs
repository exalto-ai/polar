use rusqlite::{Connection, params};
use thought_core::{Document, Position};
use thought_mcp::lineage::ProseMirrorRangeHint;
use thought_mcp::{
    ActorRef, Assurance, DocumentLineage, Ingress, LiveLineageSpan, MutationContext,
    SourceContribution, TextEdit, Workspace,
};
use thought_provenance::TextLocation;
use thought_schema::Node;
use thought_store::{Actor, InitialDocument, Origin, Store};

fn editor() -> ActorRef {
    ActorRef::editor()
}

fn claude() -> ActorRef {
    ActorRef::agent("Claude", Some("claude-sonnet"), Some("review-1"))
}

fn claude_context() -> MutationContext {
    MutationContext::mcp_reported(
        "Claude",
        Some("claude-connection".into()),
        Some("anthropic".into()),
        Some("claude-sonnet".into()),
    )
}

fn contribution<'a>(lineage: &'a DocumentLineage, label: &str) -> &'a SourceContribution {
    lineage
        .summary
        .contributions
        .iter()
        .find(|contribution| contribution.source.label == label)
        .unwrap_or_else(|| panic!("missing `{label}` contribution"))
}

fn assert_same_lineage(left: &DocumentLineage, right: &DocumentLineage) {
    assert_eq!(left.algorithm_version, right.algorithm_version);
    assert_eq!(left.alignment, right.alignment);
    assert_eq!(left.consumer_eligible, right.consumer_eligible);
    assert_eq!(left.summary, right.summary);
    assert_eq!(left.spans, right.spans);
}

fn replace_local_paragraph(local: &Document, block_id: &str, text: &str) -> Vec<u8> {
    let before = local.state_vector();
    local
        .replace_block(
            block_id,
            &Node::element("paragraph", vec![Node::text(text, vec![])]),
        )
        .unwrap();
    local.diff_since(&before)
}

fn create_two_anchor_editor_event(path: &std::path::Path) -> (String, i64) {
    let doc_id = {
        let workspace = Workspace::open(path).unwrap();
        let created = workspace
            .create_document_from_markdown_with_context(
                "Anchors bound",
                "abcd",
                &editor(),
                &MutationContext::imported(),
            )
            .unwrap();
        let local = Document::new();
        local
            .apply_update(&workspace.sync_since(&created.doc_id, &[]).unwrap())
            .unwrap();
        let block_id = local.blocks()[0].block_id.clone();
        let update = replace_local_paragraph(&local, &block_id, "aXbcYd");
        workspace
            .apply_anchored_peer_update_with_context(
                &created.doc_id,
                &update,
                &editor(),
                &MutationContext::command().with_client_event_id("two-anchor-edit".into()),
                &[
                    ProseMirrorRangeHint {
                        before_from: 2,
                        before_to: 2,
                        after_from: 2,
                        after_to: 3,
                    },
                    ProseMirrorRangeHint {
                        before_from: 4,
                        before_to: 4,
                        after_from: 5,
                        after_to: 6,
                    },
                ],
            )
            .unwrap();
        created.doc_id
    };

    let connection = Connection::open(path).unwrap();
    let event_id: i64 = connection
        .query_row(
            "SELECT event_id FROM provenance_events
             WHERE doc_id = ?1 AND client_event_id = 'two-anchor-edit'",
            [&doc_id],
            |row| row.get(0),
        )
        .unwrap();
    let anchor_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM provenance_anchors WHERE event_id = ?1",
            [event_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(anchor_count, 2);
    (doc_id, event_id)
}

#[test]
fn anchored_editor_ranges_disambiguate_repeated_text_and_rebuild_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let (doc_id, expected) = {
        let workspace = Workspace::open(&path).unwrap();
        let created = workspace.create_document("", &editor()).unwrap();
        let local = Document::new();
        local
            .apply_update(&workspace.sync_since(&created.doc_id, &[]).unwrap())
            .unwrap();
        let block_id = local.blocks()[0].block_id.clone();

        let first = replace_local_paragraph(&local, &block_id, "yes");
        workspace
            .apply_anchored_peer_update_with_context(
                &created.doc_id,
                &first,
                &editor(),
                &MutationContext::entered().with_client_event_id("entered-1".into()),
                &[ProseMirrorRangeHint {
                    before_from: 1,
                    before_to: 1,
                    after_from: 1,
                    after_to: 4,
                }],
            )
            .unwrap();

        let second = replace_local_paragraph(&local, &block_id, "yesyes");
        workspace
            .apply_anchored_peer_update_with_context(
                &created.doc_id,
                &second,
                &editor(),
                &MutationContext::pasted().with_client_event_id("paste-2".into()),
                &[ProseMirrorRangeHint {
                    before_from: 4,
                    before_to: 4,
                    after_from: 4,
                    after_to: 7,
                }],
            )
            .unwrap();

        let third = replace_local_paragraph(&local, &block_id, "yes");
        workspace
            .apply_anchored_peer_update_with_context(
                &created.doc_id,
                &third,
                &editor(),
                &MutationContext::command().with_client_event_id("delete-3".into()),
                &[ProseMirrorRangeHint {
                    before_from: 4,
                    before_to: 7,
                    after_from: 4,
                    after_to: 4,
                }],
            )
            .unwrap();

        let lineage = workspace.document_lineage(&created.doc_id).unwrap();
        assert_eq!(lineage.alignment, "anchored");
        assert!(lineage.consumer_eligible);
        assert_eq!(lineage.summary.contributions.len(), 1);
        assert_eq!(
            lineage.summary.contributions[0].source.label,
            "Written here"
        );
        assert_eq!(lineage.summary.contributions[0].graphemes, 3);
        (created.doc_id, lineage)
    };

    let connection = Connection::open(&path).unwrap();
    let versions = connection
        .prepare("SELECT chain_version FROM provenance_events WHERE doc_id = ?1 ORDER BY event_id")
        .unwrap()
        .query_map([&doc_id], |row| row.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(versions, vec![2, 2, 2, 2]);
    let anchors: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM provenance_anchors a
             JOIN provenance_events e ON e.event_id = a.event_id
             WHERE e.doc_id = ?1",
            [&doc_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(anchors, 4);
    let deleted_spans = connection
        .execute("DELETE FROM lineage_spans WHERE doc_id = ?1", [&doc_id])
        .unwrap();
    assert!(deleted_spans > 0);
    assert_eq!(
        connection
            .execute("DELETE FROM lineage_state WHERE doc_id = ?1", [&doc_id])
            .unwrap(),
        1
    );
    drop(connection);

    {
        let reopened = Workspace::open(&path).unwrap();
        assert_same_lineage(&expected, &reopened.document_lineage(&doc_id).unwrap());
    }

    let connection = Connection::open(&path).unwrap();
    let (state, rebuilt_spans): (String, i64) = connection
        .query_row(
            "SELECT
               (SELECT state FROM lineage_state WHERE doc_id = ?1),
               (SELECT COUNT(*) FROM lineage_spans WHERE doc_id = ?1)",
            [&doc_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "ready");
    assert_eq!(rebuilt_spans as usize, expected.spans.len());
}

#[test]
fn invalid_editor_ranges_fall_back_without_unlocking_consumer_percentages() {
    let workspace = Workspace::open_in_memory().unwrap();
    let created = workspace.create_document("", &editor()).unwrap();
    let local = Document::new();
    local
        .apply_update(&workspace.sync_since(&created.doc_id, &[]).unwrap())
        .unwrap();
    let block_id = local.blocks()[0].block_id.clone();
    let update = replace_local_paragraph(&local, &block_id, "fallback");

    workspace
        .apply_anchored_peer_update_with_context(
            &created.doc_id,
            &update,
            &editor(),
            &MutationContext::entered().with_client_event_id("invalid-range".into()),
            &[ProseMirrorRangeHint {
                before_from: 999,
                before_to: 999,
                after_from: 999,
                after_to: 999,
            }],
        )
        .unwrap();

    let lineage = workspace.document_lineage(&created.doc_id).unwrap();
    assert_eq!(lineage.alignment, "deterministic_inference");
    assert!(!lineage.consumer_eligible);
}

#[test]
fn active_v1_and_v2_sources_report_mixed_alignment() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let (doc_id, expected) = {
        let workspace = Workspace::open(&path).unwrap();
        let created = workspace
            .create_document_from_markdown_with_context(
                "",
                "Human",
                &editor(),
                &MutationContext::imported(),
            )
            .unwrap();
        workspace
            .replace_block_with_context(
                &created.doc_id,
                &created.blocks[0].block_id,
                "Human plus AI",
                None,
                &claude(),
                &claude_context(),
            )
            .unwrap();
        let lineage = workspace.document_lineage(&created.doc_id).unwrap();
        assert_eq!(lineage.alignment, "mixed");
        assert!(!lineage.consumer_eligible);
        (created.doc_id, lineage)
    };

    let reopened = Workspace::open(&path).unwrap();
    assert_same_lineage(&expected, &reopened.document_lineage(&doc_id).unwrap());
}

#[test]
fn concurrent_yjs_actor_inserts_remain_distinct_live_sources() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let (doc_id, expected) = {
        let workspace = Workspace::open(&path).unwrap();
        let created = workspace.create_document("", &editor()).unwrap();
        let base = workspace.sync_since(&created.doc_id, &[]).unwrap();
        let alice = Document::new();
        let bob = Document::new();
        alice.apply_update(&base).unwrap();
        bob.apply_update(&base).unwrap();
        let block_id = alice.blocks()[0].block_id.clone();
        assert_eq!(block_id, bob.blocks()[0].block_id);

        let alice_before = alice.state_vector();
        alice.append_text(&block_id, "A").unwrap();
        let alice_update = alice.diff_since(&alice_before);
        let bob_before = bob.state_vector();
        bob.append_text(&block_id, "B").unwrap();
        let bob_update = bob.diff_since(&bob_before);

        let insertion = [ProseMirrorRangeHint {
            before_from: 1,
            before_to: 1,
            after_from: 1,
            after_to: 2,
        }];
        workspace
            .apply_anchored_peer_update_with_context(
                &created.doc_id,
                &alice_update,
                &ActorRef::human("alice"),
                &MutationContext::entered().with_client_event_id("alice-concurrent".into()),
                &insertion,
            )
            .unwrap();
        workspace
            .apply_anchored_peer_update_with_context(
                &created.doc_id,
                &bob_update,
                &ActorRef::human("bob"),
                &MutationContext::pasted().with_client_event_id("bob-concurrent".into()),
                &insertion,
            )
            .unwrap();

        let view = workspace.read_document(&created.doc_id).unwrap();
        assert_eq!(view.markdown.chars().count(), 2);
        assert!(view.markdown.contains('A'));
        assert!(view.markdown.contains('B'));
        let lineage = workspace.document_lineage(&created.doc_id).unwrap();
        let written = contribution(&lineage, "Written here");
        let pasted = contribution(&lineage, "Pasted");
        assert_eq!(written.graphemes, 1);
        assert_eq!(pasted.graphemes, 1);
        assert_ne!(written.source.id, pasted.source.id);
        (created.doc_id, lineage)
    };

    let reopened = Workspace::open(&path).unwrap();
    assert_same_lineage(&expected, &reopened.document_lineage(&doc_id).unwrap());
}

#[test]
fn grammar_replacement_preserves_every_untouched_grapheme_source() {
    let workspace = Workspace::open_in_memory().unwrap();
    let writer = editor();
    let created = workspace.create_document("", &writer).unwrap();
    let block_id = created.blocks[0].block_id.clone();
    workspace
        .replace_block_with_context(
            &created.doc_id,
            &block_id,
            "It do work.",
            None,
            &writer,
            &MutationContext::entered(),
        )
        .unwrap();
    let before = workspace.document_lineage(&created.doc_id).unwrap();
    assert_eq!(before.alignment, "deterministic_inference");
    let written_source = contribution(&before, "Written here").source.id;

    workspace
        .replace_text_with_context(
            &created.doc_id,
            &block_id,
            &TextEdit {
                find: "do",
                replace: "can",
                occurrence: Some(1),
            },
            None,
            &claude(),
            &claude_context(),
        )
        .unwrap();

    let after = workspace.document_lineage(&created.doc_id).unwrap();
    let written = contribution(&after, "Written here");
    let reviewer = contribution(&after, "Claude (reported)");
    assert_eq!(after.summary.total_graphemes, 12);
    assert_eq!(after.summary.total_non_whitespace_graphemes, 10);
    assert_eq!(written.source.id, written_source);
    assert_eq!(
        (written.graphemes, written.non_whitespace_graphemes),
        (9, 7)
    );
    assert_eq!(
        (reviewer.graphemes, reviewer.non_whitespace_graphemes),
        (3, 3)
    );
    assert_eq!(
        after.spans,
        vec![
            LiveLineageSpan {
                location: TextLocation {
                    block_id: block_id.clone(),
                    path: vec![0],
                    from_utf16: 0,
                    to_utf16: 3,
                },
                source_id: written_source,
            },
            LiveLineageSpan {
                location: TextLocation {
                    block_id: block_id.clone(),
                    path: vec![0],
                    from_utf16: 3,
                    to_utf16: 6,
                },
                source_id: reviewer.source.id,
            },
            LiveLineageSpan {
                location: TextLocation {
                    block_id,
                    path: vec![0],
                    from_utf16: 6,
                    to_utf16: 12,
                },
                source_id: written_source,
            },
        ]
    );
}

#[test]
fn formatting_only_change_does_not_reassign_text() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let writer = editor();
    let workspace = Workspace::open(&path).unwrap();
    let created = workspace.create_document("", &writer).unwrap();
    let block_id = created.blocks[0].block_id.clone();
    workspace
        .replace_block_with_context(
            &created.doc_id,
            &block_id,
            "Format me.",
            None,
            &writer,
            &MutationContext::entered(),
        )
        .unwrap();
    let before = workspace.document_lineage(&created.doc_id).unwrap();

    workspace
        .replace_block_with_context(
            &created.doc_id,
            &block_id,
            "**Format me.**",
            None,
            &writer,
            &MutationContext::command(),
        )
        .unwrap();
    let after = workspace.document_lineage(&created.doc_id).unwrap();
    assert_same_lineage(&before, &after);
    let doc_id = created.doc_id;
    drop(workspace);

    let connection = Connection::open(path).unwrap();
    let operations = connection
        .prepare(
            "SELECT op FROM provenance_changes
             WHERE event_id = (
               SELECT MAX(event_id) FROM provenance_events WHERE doc_id = ?1
             ) ORDER BY ordinal",
        )
        .unwrap()
        .query_map(params![doc_id], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(operations, vec!["format"]);
}

#[test]
fn deletion_removes_live_contribution_but_keeps_a_delete_change() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let writer = editor();
    let reviewer = claude();
    let workspace = Workspace::open(&path).unwrap();
    let created = workspace.create_document("", &writer).unwrap();
    workspace
        .replace_block_with_context(
            &created.doc_id,
            &created.blocks[0].block_id,
            "Human.",
            None,
            &writer,
            &MutationContext::entered(),
        )
        .unwrap();
    let agent_block = workspace
        .insert_blocks_with_context(
            &created.doc_id,
            &Position::End,
            "Agent.",
            None,
            &reviewer,
            &claude_context(),
        )
        .unwrap()
        .block_id
        .unwrap();
    assert!(
        workspace
            .document_lineage(&created.doc_id)
            .unwrap()
            .summary
            .contributions
            .iter()
            .any(|item| item.source.label == "Claude (reported)")
    );

    workspace
        .delete_block_with_context(
            &created.doc_id,
            &agent_block,
            None,
            &writer,
            &MutationContext::command(),
        )
        .unwrap();
    let after = workspace.document_lineage(&created.doc_id).unwrap();
    assert!(
        after
            .summary
            .contributions
            .iter()
            .all(|item| item.source.label != "Claude (reported)")
    );
    let doc_id = created.doc_id;
    drop(workspace);

    let connection = Connection::open(path).unwrap();
    let reviewer_event: i64 = connection
        .query_row(
            "SELECT event_id FROM provenance_events
             WHERE doc_id = ?1 AND ingress = 'mcp'
             ORDER BY event_id DESC LIMIT 1",
            params![doc_id],
            |row| row.get(0),
        )
        .unwrap();
    let delete_rows = connection
        .prepare(
            "SELECT source_event_id, before_text FROM provenance_changes
             WHERE event_id = (
               SELECT MAX(event_id) FROM provenance_events WHERE doc_id = ?1
             ) AND op = 'delete' ORDER BY ordinal",
        )
        .unwrap()
        .query_map(params![doc_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(delete_rows, vec![(reviewer_event, "Agent.".into())]);
}

#[test]
fn entered_then_pasted_editor_content_stays_distinct() {
    let workspace = Workspace::open_in_memory().unwrap();
    let writer = editor();
    let created = workspace.create_document("", &writer).unwrap();
    workspace
        .replace_block_with_context(
            &created.doc_id,
            &created.blocks[0].block_id,
            "Typed.",
            None,
            &writer,
            &MutationContext::entered(),
        )
        .unwrap();
    workspace
        .insert_blocks_with_context(
            &created.doc_id,
            &Position::End,
            "Pasted.",
            None,
            &writer,
            &MutationContext::pasted(),
        )
        .unwrap();

    let lineage = workspace.document_lineage(&created.doc_id).unwrap();
    let entered = contribution(&lineage, "Written here");
    let pasted = contribution(&lineage, "Pasted");
    assert_eq!(entered.source.ingress, Ingress::Entered);
    assert_eq!(entered.source.assurance, Assurance::Observed);
    assert_eq!(pasted.source.ingress, Ingress::Pasted);
    assert_eq!(pasted.source.assurance, Assurance::Observed);
    assert_ne!(entered.source.id, pasted.source.id);
    assert_eq!(lineage.summary.contributions.len(), 2);
}

#[test]
fn repeated_entered_events_share_one_consumer_group_but_remain_forensic_events() {
    let workspace = Workspace::open_in_memory().unwrap();
    let writer = editor();
    let created = workspace.create_document("", &writer).unwrap();
    workspace
        .replace_block_with_context(
            &created.doc_id,
            &created.blocks[0].block_id,
            "First",
            None,
            &writer,
            &MutationContext::entered(),
        )
        .unwrap();
    workspace
        .insert_blocks_with_context(
            &created.doc_id,
            &Position::End,
            "Second",
            None,
            &writer,
            &MutationContext::entered(),
        )
        .unwrap();

    let lineage = workspace.document_lineage(&created.doc_id).unwrap();
    assert_eq!(lineage.summary.contributions.len(), 2);
    assert_ne!(
        lineage.summary.contributions[0].source.id,
        lineage.summary.contributions[1].source.id
    );
    assert_eq!(lineage.summary.grouped_contributions.len(), 1);
    let written = &lineage.summary.grouped_contributions[0];
    assert_eq!(written.group.key, "local:written");
    assert_eq!(written.group.label, "Written here");
    assert_eq!(written.event_count, 2);
    assert_eq!(written.graphemes, 11);
    assert_eq!(written.non_whitespace_graphemes, 11);
}

#[test]
fn agent_change_is_mcp_reported() {
    let workspace = Workspace::open_in_memory().unwrap();
    let writer = editor();
    let created = workspace.create_document("", &writer).unwrap();
    workspace
        .replace_block_with_context(
            &created.doc_id,
            &created.blocks[0].block_id,
            "Human draft.",
            None,
            &writer,
            &MutationContext::entered(),
        )
        .unwrap();
    workspace
        .insert_blocks_with_context(
            &created.doc_id,
            &Position::End,
            "Reviewer addition.",
            None,
            &claude(),
            &claude_context(),
        )
        .unwrap();

    let lineage = workspace.document_lineage(&created.doc_id).unwrap();
    let reviewer = contribution(&lineage, "Claude (reported)");
    assert_eq!(reviewer.source.ingress, Ingress::Mcp);
    assert_eq!(reviewer.source.assurance, Assurance::Reported);
    assert!(reviewer.non_whitespace_graphemes > 0);
}

#[test]
fn restart_returns_identical_summary_and_spans() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let writer = editor();
    let (doc_id, before_restart) = {
        let workspace = Workspace::open(&path).unwrap();
        let created = workspace.create_document("", &writer).unwrap();
        workspace
            .replace_block_with_context(
                &created.doc_id,
                &created.blocks[0].block_id,
                "Typed before restart.",
                None,
                &writer,
                &MutationContext::entered(),
            )
            .unwrap();
        workspace
            .insert_blocks_with_context(
                &created.doc_id,
                &Position::End,
                "Pasted before restart.",
                None,
                &writer,
                &MutationContext::pasted(),
            )
            .unwrap();
        let lineage = workspace.document_lineage(&created.doc_id).unwrap();
        (created.doc_id, lineage)
    };

    let reopened = Workspace::open(&path).unwrap();
    let after_restart = reopened.document_lineage(&doc_id).unwrap();
    assert_same_lineage(&before_restart, &after_restart);
}

#[test]
fn restart_preserves_frozen_mcp_label_and_connection_group() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let writer = editor();
    let reviewer = ActorRef::agent("Transport actor", Some("reported-model"), Some("run-1"));
    let context = MutationContext::mcp_reported(
        "Claude research reviewer",
        Some("connection-claude-research".into()),
        Some("anthropic".into()),
        Some("reported-model".into()),
    );

    let (doc_id, before_restart) = {
        let workspace = Workspace::open(&path).unwrap();
        let created = workspace.create_document("", &writer).unwrap();
        workspace
            .replace_block_with_context(
                &created.doc_id,
                &created.blocks[0].block_id,
                "Reviewer wording.",
                None,
                &reviewer,
                &context,
            )
            .unwrap();
        let lineage = workspace.document_lineage(&created.doc_id).unwrap();
        let reviewer = contribution(&lineage, "Claude research reviewer (reported)");
        assert_eq!(
            reviewer.source.group_key,
            "mcp:connection:connection-claude-research"
        );
        assert_ne!(reviewer.source.label, "Transport actor (reported)");
        (created.doc_id, lineage)
    };

    let reopened = Workspace::open(&path).unwrap();
    let after_restart = reopened.document_lineage(&doc_id).unwrap();
    assert_same_lineage(&before_restart, &after_restart);
    let reviewer = contribution(&after_restart, "Claude research reviewer (reported)");
    assert_eq!(
        reviewer.source.group_key,
        "mcp:connection:connection-claude-research"
    );
}

#[test]
fn deleting_only_lineage_cache_triggers_an_identical_rebuild() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let writer = editor();
    let (doc_id, before_rebuild) = {
        let workspace = Workspace::open(&path).unwrap();
        let created = workspace.create_document("", &writer).unwrap();
        workspace
            .replace_block_with_context(
                &created.doc_id,
                &created.blocks[0].block_id,
                "Human baseline.",
                None,
                &writer,
                &MutationContext::entered(),
            )
            .unwrap();
        workspace
            .insert_blocks_with_context(
                &created.doc_id,
                &Position::End,
                "AI addition.",
                None,
                &claude(),
                &claude_context(),
            )
            .unwrap();
        let lineage = workspace.document_lineage(&created.doc_id).unwrap();
        (created.doc_id, lineage)
    };

    let event_count = {
        let connection = Connection::open(&path).unwrap();
        let count = connection
            .query_row(
                "SELECT COUNT(*) FROM provenance_events WHERE doc_id = ?1",
                params![doc_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        connection
            .execute(
                "DELETE FROM lineage_spans WHERE doc_id = ?1",
                params![doc_id],
            )
            .unwrap();
        connection
            .execute(
                "DELETE FROM lineage_state WHERE doc_id = ?1",
                params![doc_id],
            )
            .unwrap();
        count
    };

    let after_rebuild = {
        let reopened = Workspace::open(&path).unwrap();
        reopened.document_lineage(&doc_id).unwrap()
    };
    assert_same_lineage(&before_rebuild, &after_rebuild);

    let connection = Connection::open(path).unwrap();
    let (state, span_count, rebuilt_event_count): (String, i64, i64) = connection
        .query_row(
            "SELECT
               (SELECT state FROM lineage_state WHERE doc_id = ?1),
               (SELECT COUNT(*) FROM lineage_spans WHERE doc_id = ?1),
               (SELECT COUNT(*) FROM provenance_events WHERE doc_id = ?1)",
            params![doc_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, "ready");
    assert_eq!(span_count as usize, after_rebuild.spans.len());
    assert_eq!(rebuilt_event_count, event_count);
}

#[test]
fn failed_provenance_insert_leaves_workspace_and_ledger_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let writer = editor();
    let workspace = Workspace::open(&path).unwrap();
    let created = workspace.create_document("", &writer).unwrap();
    let block_id = created.blocks[0].block_id.clone();
    workspace
        .replace_block_with_context(
            &created.doc_id,
            &block_id,
            "Stable before failure.",
            None,
            &writer,
            &MutationContext::entered(),
        )
        .unwrap();
    let before_view = workspace.read_document(&created.doc_id).unwrap();
    let before_lineage = workspace.document_lineage(&created.doc_id).unwrap();

    let connection = Connection::open(&path).unwrap();
    let counts = |connection: &Connection| -> (i64, i64, i64) {
        connection
            .query_row(
                "SELECT
                   (SELECT COUNT(*) FROM updates WHERE doc_id = ?1),
                   (SELECT COUNT(*) FROM provenance_events WHERE doc_id = ?1),
                   (SELECT COUNT(*) FROM provenance_changes c
                      JOIN provenance_events e ON e.event_id = c.event_id
                      WHERE e.doc_id = ?1)",
                params![created.doc_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    };
    let before_counts = counts(&connection);
    connection
        .execute_batch(
            "CREATE TRIGGER test_abort_provenance_change
             BEFORE INSERT ON provenance_changes
             BEGIN
               SELECT RAISE(ABORT, 'intentional provenance test failure');
             END;",
        )
        .unwrap();

    let failed = workspace.replace_text_with_context(
        &created.doc_id,
        &block_id,
        &TextEdit {
            find: "Stable",
            replace: "Changed",
            occurrence: Some(1),
        },
        None,
        &writer,
        &MutationContext::entered(),
    );
    assert!(failed.is_err());

    let after_view = workspace.read_document(&created.doc_id).unwrap();
    let after_lineage = workspace.document_lineage(&created.doc_id).unwrap();
    let after_counts = counts(&connection);
    connection
        .execute_batch("DROP TRIGGER test_abort_provenance_change;")
        .unwrap();

    assert_eq!(after_view.markdown, before_view.markdown);
    assert_eq!(after_view.version, before_view.version);
    assert_same_lineage(&before_lineage, &after_lineage);
    assert_eq!(after_counts, before_counts);
}

#[test]
fn legacy_store_document_seeds_legacy_unknown_without_inferring_actor() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let doc_id = "legacy-document";
    {
        let store = Store::open(&path).unwrap();
        store
            .upsert_actor(&Actor {
                id: "agent:historical".into(),
                kind: "agent".into(),
                display_name: "Historical Claude".into(),
                model: Some("old-model".into()),
                color: "#000000".into(),
            })
            .unwrap();
        let document = Document::new();
        document.set_document(&Node::element(
            "doc",
            vec![Node::element(
                "paragraph",
                vec![Node::text("Legacy words.", vec![])],
            )],
        ));
        let block_ids = document
            .blocks()
            .into_iter()
            .map(|block| block.block_id)
            .collect::<Vec<_>>();
        store
            .create_initial_document(InitialDocument {
                id: doc_id,
                title: "Legacy",
                payload: &document.encode_state(),
                actor_id: "agent:historical",
                origin: Origin::Agent,
                session_id: Some("historical-run"),
                markdown: "Legacy words.",
                block_ids: &block_ids,
                attributed_at: 1,
            })
            .unwrap();
    }

    let lineage = {
        let workspace = Workspace::open(&path).unwrap();
        workspace.document_lineage(doc_id).unwrap()
    };
    assert_eq!(lineage.summary.contributions.len(), 1);
    let source = &lineage.summary.contributions[0];
    assert_eq!(source.source.label, "Legacy content");
    assert_eq!(source.source.ingress, Ingress::LegacyUnknown);
    assert_eq!(source.source.assurance, Assurance::Unknown);
    assert_eq!(source.graphemes, "Legacy words.".chars().count());

    let connection = Connection::open(path).unwrap();
    let persisted: (String, String, String, Option<String>) = connection
        .query_row(
            "SELECT action, ingress, assurance, actor_id
             FROM provenance_events WHERE doc_id = ?1",
            params![doc_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        persisted,
        (
            "legacy_seed".into(),
            "legacy_unknown".into(),
            "unknown".into(),
            None,
        )
    );
}

#[test]
fn empty_document_has_ready_state_and_zero_spans() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let doc_id = {
        let workspace = Workspace::open(&path).unwrap();
        let created = workspace.create_document("", &editor()).unwrap();
        let lineage = workspace.document_lineage(&created.doc_id).unwrap();
        assert_eq!(lineage.summary.total_graphemes, 0);
        assert_eq!(lineage.summary.total_non_whitespace_graphemes, 0);
        assert!(lineage.summary.contributions.is_empty());
        assert!(lineage.spans.is_empty());
        created.doc_id
    };

    let connection = Connection::open(path).unwrap();
    let (state, span_count): (String, i64) = connection
        .query_row(
            "SELECT
               (SELECT state FROM lineage_state WHERE doc_id = ?1),
               (SELECT COUNT(*) FROM lineage_spans WHERE doc_id = ?1)",
            params![doc_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "ready");
    assert_eq!(span_count, 0);
}

#[test]
fn failed_compatibility_backfill_never_leaves_a_document_only_cache() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let doc_id = "retryable-legacy-document";
    {
        let store = Store::open(&path).unwrap();
        store
            .upsert_actor(&Actor {
                id: "human:legacy".into(),
                kind: "human".into(),
                display_name: "Legacy Writer".into(),
                model: None,
                color: "#000000".into(),
            })
            .unwrap();
        let document = Document::new();
        document.set_document(&Node::element(
            "doc",
            vec![Node::element(
                "paragraph",
                vec![Node::text("Retry safely.", vec![])],
            )],
        ));
        let block_ids = document
            .blocks()
            .into_iter()
            .map(|block| block.block_id)
            .collect::<Vec<_>>();
        store
            .create_initial_document(InitialDocument {
                id: doc_id,
                title: "Retry safely",
                payload: &document.encode_state(),
                actor_id: "human:legacy",
                origin: Origin::Human,
                session_id: None,
                markdown: "Retry safely.",
                block_ids: &block_ids,
                attributed_at: 1,
            })
            .unwrap();
    }

    let connection = Connection::open(&path).unwrap();
    connection
        .execute("DELETE FROM block_provenance WHERE doc_id = ?1", [doc_id])
        .unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER test_abort_compatibility_backfill
             BEFORE INSERT ON block_provenance
             BEGIN
               SELECT RAISE(ABORT, 'intentional backfill failure');
             END;",
        )
        .unwrap();

    let workspace = Workspace::open(&path).unwrap();
    for _ in 0..2 {
        let error = workspace.read_document(doc_id).unwrap_err().to_string();
        assert!(error.contains("intentional backfill failure"));
    }

    connection
        .execute_batch("DROP TRIGGER test_abort_compatibility_backfill;")
        .unwrap();
    assert_eq!(workspace.read_document(doc_id).unwrap().doc_id, doc_id);
    assert_eq!(
        contribution(
            &workspace.document_lineage(doc_id).unwrap(),
            "Legacy content"
        )
        .graphemes,
        "Retry safely.".chars().count()
    );
}

#[test]
fn event_chain_binds_tombstones_and_the_immutable_update_log() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let doc_id = {
        let workspace = Workspace::open(&path).unwrap();
        let created = workspace
            .create_document("Replicated state", &editor())
            .unwrap();
        workspace
            .set_document_deleted(&created.doc_id, true, &editor())
            .unwrap();
        workspace
            .set_document_deleted(&created.doc_id, false, &editor())
            .unwrap();
        created.doc_id
    };

    let connection = Connection::open(&path).unwrap();
    let hashes = connection
        .prepare(
            "SELECT action, before_hash, after_hash, update_log_root
             FROM provenance_events WHERE doc_id = ?1 ORDER BY event_id",
        )
        .unwrap()
        .query_map([&doc_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(hashes.len(), 3);
    assert_eq!(
        hashes.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
        ["edit", "trash", "restore"]
    );
    assert_eq!(hashes[1].1, hashes[0].2);
    assert_ne!(hashes[1].1, hashes[1].2, "trash changes replicated state");
    assert_eq!(hashes[2].1, hashes[1].2);
    assert_eq!(hashes[2].2, hashes[0].2, "restore returns to live state");
    assert!(hashes.iter().all(|row| row.3.len() == 32));
    assert_ne!(hashes[0].3, hashes[1].3);
    assert_ne!(hashes[1].3, hashes[2].3);

    connection
        .execute(
            "INSERT INTO actors (id, kind, display_name, color, first_seen)
             VALUES ('human:tamper', 'human', 'Tamper', '#000000', 1)",
            [],
        )
        .unwrap();
    connection
        .execute_batch("DROP TRIGGER updates_reject_evidence_update;")
        .unwrap();
    connection
        .execute(
            "UPDATE updates SET actor_id = 'human:tamper'
             WHERE doc_id = ?1 AND seq = (SELECT MIN(seq) FROM updates WHERE doc_id = ?1)",
            [&doc_id],
        )
        .unwrap();
    drop(connection);

    let reopened = Workspace::open(&path).unwrap();
    for _ in 0..2 {
        let error = reopened.read_document(&doc_id).unwrap_err().to_string();
        assert!(error.contains("invalid update log root"));
    }
}

#[test]
fn restart_rejects_an_update_origin_changed_to_unknown_raw_text() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let doc_id = {
        let workspace = Workspace::open(&path).unwrap();
        workspace
            .create_document("Origin bound", &editor())
            .unwrap()
            .doc_id
    };

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP TRIGGER updates_reject_evidence_update;")
        .unwrap();
    connection
        .execute(
            "UPDATE updates SET origin = 'bogus' WHERE doc_id = ?1",
            [&doc_id],
        )
        .unwrap();
    drop(connection);

    let reopened = Workspace::open(&path).unwrap();
    for _ in 0..2 {
        let error = reopened.read_document(&doc_id).unwrap_err().to_string();
        assert!(error.contains("invalid update log root"));
    }
}

#[test]
fn restart_rejects_tampered_anchor_material() {
    for tamper in ["altered", "removed", "reordered"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("thought.db");
        let (doc_id, event_id) = create_two_anchor_editor_event(&path);
        let connection = Connection::open(&path).unwrap();

        match tamper {
            "altered" => {
                // A same-size digest passes the table CHECK, so bypass only the
                // append-only guard and let replay detect the changed evidence.
                connection
                    .execute_batch("DROP TRIGGER provenance_anchors_reject_update;")
                    .unwrap();
                connection
                    .execute(
                        "UPDATE provenance_anchors
                         SET after_text_hash = zeroblob(32)
                         WHERE event_id = ?1 AND ordinal = 0",
                        [event_id],
                    )
                    .unwrap();
            }
            "removed" => {
                // Removing one row preserves the V2 nonempty shape and foreign
                // keys. Verification must still reject the shortened hash input.
                connection
                    .execute_batch("DROP TRIGGER provenance_anchors_reject_delete;")
                    .unwrap();
                assert_eq!(
                    connection
                        .execute(
                            "DELETE FROM provenance_anchors
                             WHERE event_id = ?1 AND ordinal = 1",
                            [event_id],
                        )
                        .unwrap(),
                    1
                );
            }
            "reordered" => {
                // Ordinals cannot be swapped directly because the composite
                // primary key and insertion-order trigger reject that shape.
                // Swap the complete material behind ordinals 0 and 1 instead.
                connection
                    .execute_batch(
                        "DROP TRIGGER provenance_anchors_reject_update;
                         CREATE TEMP TABLE saved_anchor_material AS
                         SELECT ordinal, basis,
                                before_start_grapheme, before_end_grapheme,
                                after_start_grapheme, after_end_grapheme,
                                before_text_hash, after_text_hash
                         FROM provenance_anchors WHERE 0;",
                    )
                    .unwrap();
                connection
                    .execute(
                        "INSERT INTO saved_anchor_material
                         SELECT ordinal, basis,
                                before_start_grapheme, before_end_grapheme,
                                after_start_grapheme, after_end_grapheme,
                                before_text_hash, after_text_hash
                         FROM provenance_anchors WHERE event_id = ?1",
                        [event_id],
                    )
                    .unwrap();
                connection
                    .execute(
                        "UPDATE provenance_anchors AS target
                         SET (
                           basis,
                           before_start_grapheme, before_end_grapheme,
                           after_start_grapheme, after_end_grapheme,
                           before_text_hash, after_text_hash
                         ) = (
                           SELECT basis,
                                  before_start_grapheme, before_end_grapheme,
                                  after_start_grapheme, after_end_grapheme,
                                  before_text_hash, after_text_hash
                           FROM saved_anchor_material AS source
                           WHERE source.ordinal = 1 - target.ordinal
                         )
                         WHERE target.event_id = ?1 AND target.ordinal IN (0, 1)",
                        [event_id],
                    )
                    .unwrap();
                connection
                    .execute_batch("DROP TABLE saved_anchor_material;")
                    .unwrap();
            }
            _ => unreachable!(),
        }
        drop(connection);

        let reopened = Workspace::open(&path).unwrap();
        for _ in 0..2 {
            let error = reopened.read_document(&doc_id).unwrap_err().to_string();
            let expected = match tamper {
                "altered" => "invalid event hash",
                "removed" => "invalid event hash",
                "reordered" => "unordered or overlapping",
                _ => unreachable!(),
            };
            assert!(
                error.contains(expected),
                "{tamper} anchor material produced an unexpected verification error: {error}"
            );
        }
    }
}

#[test]
fn restart_rejects_an_unsupported_event_chain_version() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let doc_id = {
        let workspace = Workspace::open(&path).unwrap();
        workspace
            .create_document("Version bound", &editor())
            .unwrap()
            .doc_id
    };

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP TRIGGER provenance_events_reject_update;
             PRAGMA ignore_check_constraints = ON;",
        )
        .unwrap();
    connection
        .execute(
            "UPDATE provenance_events SET chain_version = 3 WHERE doc_id = ?1",
            [&doc_id],
        )
        .unwrap();
    connection
        .execute_batch("PRAGMA ignore_check_constraints = OFF;")
        .unwrap();
    drop(connection);

    let reopened = Workspace::open(&path).unwrap();
    for _ in 0..2 {
        let error = reopened.read_document(&doc_id).unwrap_err().to_string();
        assert!(error.contains("unsupported chain version 3"));
    }
}
