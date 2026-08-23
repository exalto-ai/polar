//! M1.0, verbatim. Every one of these runs with no window, no editor, and no
//! HTTP — which is the point. If AD-2 is right, the daemon is complete without
//! a UI, and this file is what proves it rather than asserting it.

use polar_core::Position;
use polar_mcp::{ActorRef, Workspace};

fn agent() -> ActorRef {
    ActorRef::agent("opus", Some("claude-opus-5"), Some("run-1"))
}

/// M1.0 (1) — an agent creates a document, reads it as markdown with block
/// anchors, edits a block, and reads back its own edit.
#[test]
fn an_agent_can_create_read_and_edit_a_document() {
    let ws = Workspace::open_in_memory().unwrap();
    let agent = agent();

    let created = ws.create_document("Notes", &agent).unwrap();
    assert_eq!(
        created.blocks.len(),
        1,
        "a new document starts with one block"
    );

    let first = created.blocks[0].block_id.clone();
    ws.replace_block(
        &created.doc_id,
        &first,
        "# Roadmap\n\nThe first paragraph.",
        Some(&created.version),
        &agent,
    )
    .unwrap();

    let view = ws.read_document(&created.doc_id).unwrap();
    assert!(view.markdown.contains("# Roadmap"));
    assert!(view.markdown.contains("The first paragraph."));
    assert_eq!(
        view.title, "Roadmap",
        "title is derived from the first heading"
    );

    // Anchors must address real lines, or a follow-up edit lands elsewhere.
    let lines: Vec<&str> = view.markdown.lines().collect();
    for block in &view.blocks {
        assert!(block.line_start >= 1 && block.line_end <= lines.len());
        assert!(!block.block_id.is_empty());
    }

    // And the agent can address what it just wrote.
    let heading = view.blocks.iter().find(|b| b.kind == "heading").unwrap();
    ws.replace_block(
        &view.doc_id,
        &heading.block_id,
        "# Roadmap (revised)",
        Some(&view.version),
        &agent,
    )
    .unwrap();
    assert!(
        ws.read_document(&view.doc_id)
            .unwrap()
            .markdown
            .contains("# Roadmap (revised)")
    );
}

/// M1.0 (2) — attribution is correct and survives a restart.
#[test]
fn attribution_is_correct_and_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("polar.db");

    let human = ActorRef::human("kev");
    let agent = agent();
    let (doc_id, markdown_before) = {
        let ws = Workspace::open(&path).unwrap();
        let created = ws.create_document("Shared", &human).unwrap();
        let block = created.blocks[0].block_id.clone();

        ws.replace_block(&created.doc_id, &block, "Written by a human.", None, &human)
            .unwrap();
        ws.insert_blocks(
            &created.doc_id,
            &Position::End,
            "Appended by an agent.",
            None,
            &agent,
        )
        .unwrap();

        let view = ws.read_document(&created.doc_id).unwrap();
        (created.doc_id, view.markdown)
    }; // workspace dropped: the daemon has exited

    let ws = Workspace::open(&path).unwrap();
    let view = ws.read_document(&doc_id).unwrap();
    assert_eq!(
        view.markdown, markdown_before,
        "document changed across restart"
    );

    let log = ws.attribution(&doc_id).unwrap();
    assert_eq!(log.len(), 3, "create + two edits");
    assert_eq!(log[0].0, "human:kev");
    assert_eq!(log[1].0, "human:kev");
    assert_eq!(log[2].0, "agent:opus");
    // The agent's run is keyed, so it can be reverted as a unit (AD-11).
    assert_eq!(log[2].1.as_deref(), Some("run-1"));
    assert_eq!(log[1].1, None, "human edits carry no agent session");
}

/// M1.0 (4) — two agents editing the same document converge, and both edits
/// stay individually attributable.
#[test]
fn two_agents_editing_converge_and_stay_attributable() {
    let ws = Workspace::open_in_memory().unwrap();
    let alice = ActorRef::agent("alice", Some("claude-opus-5"), Some("alice-run"));
    let bob = ActorRef::agent("bob", Some("claude-sonnet-5"), Some("bob-run"));

    let created = ws.create_document("Collab", &alice).unwrap();
    let first = created.blocks[0].block_id.clone();
    ws.replace_block(&created.doc_id, &first, "Opening line.", None, &alice)
        .unwrap();

    // Both read the same version, then both write against it.
    let shared = ws.read_document(&created.doc_id).unwrap();
    let target = shared.blocks[0].block_id.clone();

    ws.insert_blocks(
        &shared.doc_id,
        &Position::After(target.clone()),
        "Alice's addition.",
        Some(&shared.version),
        &alice,
    )
    .unwrap();

    // Bob's version token is now stale. That must warn, not fail (M1.6).
    let bob_edit = ws
        .insert_blocks(
            &shared.doc_id,
            &Position::After(target),
            "Bob's addition.",
            Some(&shared.version),
            &bob,
        )
        .unwrap();
    assert!(
        !bob_edit.warnings.is_empty(),
        "a stale read must be reported to the agent"
    );

    let view = ws.read_document(&shared.doc_id).unwrap();
    assert!(view.markdown.contains("Opening line."), "lost the original");
    assert!(
        view.markdown.contains("Alice's addition."),
        "lost Alice's edit"
    );
    assert!(view.markdown.contains("Bob's addition."), "lost Bob's edit");

    let actors: Vec<String> = ws
        .attribution(&shared.doc_id)
        .unwrap()
        .into_iter()
        .map(|(a, _)| a)
        .collect();
    assert!(actors.contains(&"agent:alice".to_string()));
    assert!(actors.contains(&"agent:bob".to_string()));
}

/// The tool surface must refuse markdown that would produce an invalid tree,
/// rather than storing it and failing later somewhere less obvious.
#[test]
fn invalid_payloads_are_refused_at_the_boundary() {
    let ws = Workspace::open_in_memory().unwrap();
    let agent = agent();
    let created = ws.create_document("Notes", &agent).unwrap();
    let block = created.blocks[0].block_id.clone();

    let empty = ws.replace_block(&created.doc_id, &block, "", None, &agent);
    assert!(empty.is_err(), "empty markdown produces no blocks");

    // A block that has since vanished reports news, not a crash.
    ws.delete_block(&created.doc_id, &block, None, &agent)
        .unwrap();
    let gone = ws.replace_block(&created.doc_id, &block, "text", None, &agent);
    assert!(gone.is_err());
    assert!(format!("{}", gone.unwrap_err()).contains("re-read"));
}

/// Search is what keeps agents from reading every document to find one.
#[test]
fn search_reflects_edits_immediately() {
    let ws = Workspace::open_in_memory().unwrap();
    let agent = agent();
    let created = ws.create_document("Notes", &agent).unwrap();
    let block = created.blocks[0].block_id.clone();

    ws.replace_block(
        &created.doc_id,
        &block,
        "The quarterly roadmap review is on Tuesday.",
        None,
        &agent,
    )
    .unwrap();

    let hits = ws.search("roadmap", 10).unwrap();
    assert_eq!(hits.len(), 1, "the index must not lag the edit");
    assert_eq!(hits[0].doc_id, created.doc_id);
}

/// `replace_text` matches the block's markdown, because markdown is what the
/// agent read. Matching against rendered text it never saw would make failures
/// inexplicable.
#[test]
fn replace_text_edits_within_a_block() {
    let ws = Workspace::open_in_memory().unwrap();
    let agent = agent();
    let created = ws.create_document("Notes", &agent).unwrap();
    let block = created.blocks[0].block_id.clone();

    ws.replace_block(
        &created.doc_id,
        &block,
        "ship on Tuesday, review on Tuesday",
        None,
        &agent,
    )
    .unwrap();
    let block = ws.read_document(&created.doc_id).unwrap().blocks[0]
        .block_id
        .clone();

    // Targeting one occurrence leaves the others alone.
    ws.replace_text(
        &created.doc_id,
        &block,
        "Tuesday",
        "Thursday",
        Some(2),
        None,
        &agent,
    )
    .unwrap();
    let md = ws.read_document(&created.doc_id).unwrap().markdown;
    assert_eq!(md, "ship on Tuesday, review on Thursday");

    // Omitting the occurrence replaces every match.
    let block = ws.read_document(&created.doc_id).unwrap().blocks[0]
        .block_id
        .clone();
    ws.replace_text(&created.doc_id, &block, "on", "before", None, None, &agent)
        .unwrap();
    assert_eq!(
        ws.read_document(&created.doc_id).unwrap().markdown,
        "ship before Tuesday, review before Thursday"
    );

    // A miss is reported rather than silently doing nothing, which would leave
    // an agent believing it had made an edit.
    let block = ws.read_document(&created.doc_id).unwrap().blocks[0]
        .block_id
        .clone();
    let missing = ws.replace_text(&created.doc_id, &block, "Saturday", "x", None, None, &agent);
    assert!(missing.is_err());
    assert!(format!("{}", missing.unwrap_err()).contains("does not appear"));

    let past_end = ws.replace_text(
        &created.doc_id,
        &block,
        "before",
        "x",
        Some(9),
        None,
        &agent,
    );
    assert!(past_end.is_err());
}
