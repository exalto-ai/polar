use rusqlite::Connection;
use std::time::{Duration, Instant};
use thought_core::Document;
use thought_mcp::lineage::ProseMirrorRangeHint;
use thought_mcp::{ActorRef, MutationContext, Workspace};

const WORDS: usize = 10_000;
const HISTORY_EVENTS: usize = 100;
const INTERACTIVE_BUDGET: Duration = Duration::from_millis(100);
const COLD_OPEN_BUDGET: Duration = Duration::from_millis(150);
// This is a cache-repair path, not normal cold open. Replaying and verifying
// every historical state is deliberately allowed a much wider budget.
const RECOVERY_REBUILD_BUDGET: Duration = Duration::from_secs(10);

fn append_anchored(
    workspace: &Workspace,
    local: &Document,
    doc_id: &str,
    block_id: &str,
    before_position: u32,
    client_event_id: String,
) {
    let before = local.state_vector();
    local.append_text(block_id, "x").unwrap();
    let update = local.diff_since(&before);
    workspace
        .apply_anchored_peer_update_with_context(
            doc_id,
            &update,
            &ActorRef::editor(),
            &MutationContext::entered().with_client_event_id(client_event_id),
            &[ProseMirrorRangeHint {
                before_from: before_position,
                before_to: before_position,
                after_from: before_position,
                after_to: before_position + 1,
            }],
        )
        .unwrap();
}

/// Reference-machine performance gate, intentionally opt-in because shared CI
/// runners cannot make useful wall-clock promises. Run with:
///
/// `cargo test --release -p thought-mcp --test provenance_performance -- --ignored --nocapture`
#[test]
#[ignore = "reference-machine wall-clock gate"]
fn large_document_typing_cold_open_and_recovery_meet_reference_budgets() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thought.db");
    let text = (0..WORDS)
        .map(|index| format!("word{index:05}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut position = u32::try_from(text.encode_utf16().count()).unwrap() + 1;

    let doc_id = {
        let workspace = Workspace::open(&path).unwrap();
        let created = workspace
            .create_document_from_markdown_with_context(
                "Performance fixture",
                &text,
                &ActorRef::editor(),
                &MutationContext::imported(),
            )
            .unwrap();
        let local = Document::new();
        local
            .apply_update(&workspace.sync_since(&created.doc_id, &[]).unwrap())
            .unwrap();
        let block_id = local.blocks()[0].block_id.clone();
        for event in 0..HISTORY_EVENTS {
            append_anchored(
                &workspace,
                &local,
                &created.doc_id,
                &block_id,
                position,
                format!("history-{event}"),
            );
            position += 1;
        }

        let started = Instant::now();
        append_anchored(
            &workspace,
            &local,
            &created.doc_id,
            &block_id,
            position,
            "measured-interactive".into(),
        );
        let interactive = started.elapsed();
        eprintln!("anchored interactive commit: {interactive:?}");
        assert!(
            interactive <= INTERACTIVE_BUDGET,
            "anchored commit took {interactive:?}, budget is {INTERACTIVE_BUDGET:?}"
        );
        created.doc_id
    };

    let started = Instant::now();
    let reopened = Workspace::open(&path).unwrap();
    let lineage = reopened.document_lineage(&doc_id).unwrap();
    let cold_open = started.elapsed();
    eprintln!("cached cold open: {cold_open:?}");
    assert!(lineage.consumer_eligible);
    assert!(
        cold_open <= COLD_OPEN_BUDGET,
        "cached cold open took {cold_open:?}, budget is {COLD_OPEN_BUDGET:?}"
    );
    drop(reopened);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute("DELETE FROM lineage_spans WHERE doc_id = ?1", [&doc_id])
        .unwrap();
    connection
        .execute("DELETE FROM lineage_state WHERE doc_id = ?1", [&doc_id])
        .unwrap();
    drop(connection);

    let started = Instant::now();
    let rebuilt = Workspace::open(&path).unwrap();
    let lineage = rebuilt.document_lineage(&doc_id).unwrap();
    let recovery = started.elapsed();
    eprintln!("101-event recovery rebuild: {recovery:?}");
    assert!(lineage.consumer_eligible);
    assert!(
        recovery <= RECOVERY_REBUILD_BUDGET,
        "recovery rebuild took {recovery:?}, budget is {RECOVERY_REBUILD_BUDGET:?}"
    );
}
