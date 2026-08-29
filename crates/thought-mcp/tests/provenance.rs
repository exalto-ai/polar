//! Documents here are created unnamed on purpose: a named one is seeded with
//! its title as a heading, and these tests are about how blocks are attributed
//! rather than about that heading.
//!
//! Per-block attribution: who wrote which paragraph.
//!
//! The op log answers "who wrote" per *update*; these pin the per-*block*
//! answer the rails are drawn from, including the two cases that are easy to
//! get wrong — an actor touching one block must not re-attribute the others,
//! and a document written before the table existed must attribute itself.

use std::collections::HashMap;
use thought_core::Position;
use thought_mcp::{ActorRef, MutationContext, Workspace};

fn agent(name: &str) -> ActorRef {
    ActorRef::agent(name, Some("claude-opus-5"), Some(&format!("run-{name}")))
}

fn human() -> ActorRef {
    ActorRef::human("editor")
}

fn reviewer(model: &str, session: &str) -> ActorRef {
    ActorRef::reviewer(
        "reviewer-route-1",
        "Configured reviewer",
        Some(model),
        Some(session),
    )
}

fn reviewer_context(model: &str) -> MutationContext {
    let _ = model;
    MutationContext::mcp_connection("Configured for Codex (reported)", "reviewer-route-1")
}

/// block_id -> who last touched it.
fn touched(ws: &Workspace, doc_id: &str) -> HashMap<String, String> {
    ws.block_provenance(doc_id)
        .unwrap()
        .into_iter()
        .map(|b| (b.block_id, b.touched_by))
        .collect()
}

/// Creation does not go through the commit path, so it has to attribute its own
/// first block. When it did not, the baseline was empty and the next actor to
/// write inherited a paragraph it never wrote.
#[test]
fn the_block_a_new_document_starts_with_belongs_to_whoever_made_it() {
    let ws = Workspace::open_in_memory().unwrap();
    let doc = ws.create_document("", &human()).unwrap();
    let first = doc.blocks[0].block_id.clone();

    assert_eq!(
        touched(&ws, &doc.doc_id).get(&first).map(String::as_str),
        Some("human:editor"),
        "the empty paragraph belongs to the window that made the document"
    );

    // And an agent writing next must not inherit it.
    ws.insert_blocks(
        &doc.doc_id,
        &Position::End,
        "From the agent.",
        None,
        &agent("opus"),
    )
    .unwrap();
    assert_eq!(
        touched(&ws, &doc.doc_id).get(&first).map(String::as_str),
        Some("human:editor"),
        "a later writer does not inherit the blocks it never touched"
    );
}

#[test]
fn each_block_is_attributed_to_whoever_wrote_it() {
    let ws = Workspace::open_in_memory().unwrap();
    let opus = agent("opus");
    let doc = ws.create_document("", &opus).unwrap();

    let first = doc.blocks[0].block_id.clone();
    ws.replace_block(&doc.doc_id, &first, "# Roadmap", None, &opus)
        .unwrap();
    ws.insert_blocks(
        &doc.doc_id,
        &Position::End,
        "Written by the agent.",
        None,
        &opus,
    )
    .unwrap();

    // A human adds a third block. Only that block should become theirs.
    let out = ws
        .insert_blocks(
            &doc.doc_id,
            &Position::End,
            "Written by hand.",
            None,
            &human(),
        )
        .unwrap();
    let by_hand = out.block_id.unwrap();

    let who = touched(&ws, &doc.doc_id);
    assert_eq!(who.len(), 3, "every block is attributed");
    assert_eq!(who[&by_hand], "human:editor");
    assert_eq!(
        who.values().filter(|w| *w == "agent:opus").count(),
        2,
        "the agent keeps the two blocks it wrote"
    );
}

#[test]
fn editing_one_block_leaves_the_others_attributed_to_whoever_wrote_them() {
    let ws = Workspace::open_in_memory().unwrap();
    let opus = agent("opus");
    let doc = ws.create_document("", &opus).unwrap();

    ws.replace_block(
        &doc.doc_id,
        &doc.blocks[0].block_id,
        "# Roadmap\n\nOne.\n\nTwo.\n\nThree.",
        None,
        &opus,
    )
    .unwrap();

    let view = ws.read_document(&doc.doc_id).unwrap();
    let second = view.blocks[1].block_id.clone();
    ws.replace_text(
        &doc.doc_id,
        &second,
        &thought_mcp::TextEdit {
            find: "One.",
            replace: "One, reworded.",
            occurrence: None,
        },
        None,
        &human(),
    )
    .unwrap();

    let who = touched(&ws, &doc.doc_id);
    assert_eq!(
        who[&second], "human:editor",
        "the edited block belongs to whoever edited it"
    );
    let agent_blocks = who.values().filter(|w| *w == "agent:opus").count();
    assert_eq!(
        agent_blocks, 3,
        "the untouched blocks stay with the agent; a one-word edit is not a rewrite"
    );
}

#[test]
fn a_reworded_block_remembers_who_drafted_it() {
    let ws = Workspace::open_in_memory().unwrap();
    let opus = agent("opus");
    let doc = ws.create_document("", &opus).unwrap();
    let first = doc.blocks[0].block_id.clone();

    ws.replace_block(&doc.doc_id, &first, "A drafted sentence.", None, &opus)
        .unwrap();
    ws.replace_block(&doc.doc_id, &first, "A reworded sentence.", None, &human())
        .unwrap();

    let block = ws
        .block_provenance(&doc.doc_id)
        .unwrap()
        .into_iter()
        .find(|b| b.block_id == first)
        .expect("the block is attributed");

    assert_eq!(block.created_by, "agent:opus", "the agent drafted it");
    assert_eq!(block.touched_by, "human:editor", "the human reworded it");
}

#[test]
fn a_later_model_on_another_document_cannot_rewrite_block_history() {
    let ws = Workspace::open_in_memory().unwrap();
    let model_a = reviewer("model-a", "turn-a");
    let first = ws
        .create_document_from_markdown_with_context(
            "",
            "Earlier wording.",
            &model_a,
            &reviewer_context("model-a"),
        )
        .unwrap();
    let first_block = first.blocks[0].block_id.clone();

    let model_b = reviewer("model-b", "turn-b");
    let second = ws
        .create_document_from_markdown_with_context(
            "",
            "Later wording in another document.",
            &model_b,
            &reviewer_context("model-b"),
        )
        .unwrap();

    let first_attribution = ws
        .block_provenance(&first.doc_id)
        .unwrap()
        .into_iter()
        .find(|block| block.block_id == first_block)
        .unwrap();
    let second_attribution = ws.block_provenance(&second.doc_id).unwrap();
    assert_eq!(first_attribution.model, None);
    assert_eq!(second_attribution[0].model, None);

    let first_actor = ws
        .document_actors(&first.doc_id)
        .unwrap()
        .into_iter()
        .find(|actor| actor.actor_id == "reviewer:reviewer-route-1")
        .unwrap();
    assert_eq!(first_actor.model, None);
}

#[test]
fn a_later_model_on_the_same_document_cannot_rewrite_an_untouched_block() {
    let ws = Workspace::open_in_memory().unwrap();
    let model_a = reviewer("model-a", "turn-a");
    let doc = ws
        .create_document_from_markdown_with_context(
            "",
            "Earlier block.\n\nSecond block.",
            &model_a,
            &reviewer_context("model-a"),
        )
        .unwrap();
    let earlier_block = doc.blocks[0].block_id.clone();
    let changed_block = doc.blocks[1].block_id.clone();

    let model_b = reviewer("model-b", "turn-b");
    ws.replace_block_with_context(
        &doc.doc_id,
        &changed_block,
        "Second block, revised.",
        None,
        &model_b,
        &reviewer_context("model-b"),
    )
    .unwrap();

    let by_id = ws
        .block_provenance(&doc.doc_id)
        .unwrap()
        .into_iter()
        .map(|block| (block.block_id.clone(), block))
        .collect::<HashMap<_, _>>();
    assert_eq!(by_id[&earlier_block].model, None);
    assert_eq!(by_id[&changed_block].model, None);

    let actor = ws
        .document_actors(&doc.doc_id)
        .unwrap()
        .into_iter()
        .find(|actor| actor.actor_id == "reviewer:reviewer-route-1")
        .unwrap();
    assert_eq!(actor.model, None, "a multi-model summary is ambiguous");
}

#[test]
fn a_deleted_block_stops_being_attributed() {
    let ws = Workspace::open_in_memory().unwrap();
    let opus = agent("opus");
    let doc = ws.create_document("", &opus).unwrap();

    ws.replace_block(
        &doc.doc_id,
        &doc.blocks[0].block_id,
        "One.\n\nTwo.",
        None,
        &opus,
    )
    .unwrap();
    let view = ws.read_document(&doc.doc_id).unwrap();
    assert_eq!(touched(&ws, &doc.doc_id).len(), 2);

    ws.delete_block(&doc.doc_id, &view.blocks[1].block_id, None, &human())
        .unwrap();

    let who = touched(&ws, &doc.doc_id);
    assert_eq!(who.len(), 1, "the deleted block leaves the table with it");
    assert!(!who.contains_key(&view.blocks[1].block_id));
}

/// The backfill path: a document whose log predates the provenance table is
/// attributed by replay, not left blank forever.
#[test]
fn a_document_written_before_the_table_existed_attributes_itself() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("thought.db");

    let opus = agent("opus");
    let doc_id = {
        let ws = Workspace::open(&path).unwrap();
        let doc = ws.create_document("", &opus).unwrap();
        ws.replace_block(
            &doc.doc_id,
            &doc.blocks[0].block_id,
            "# Roadmap\n\nOne.",
            None,
            &opus,
        )
        .unwrap();
        ws.insert_blocks(&doc.doc_id, &Position::End, "Two.", None, &human())
            .unwrap();
        doc.doc_id
    };

    // Wipe the derived table, exactly as a document from before it would look,
    // and confirm a plain read rebuilds it from the log.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute("DELETE FROM block_provenance", []).unwrap();
    drop(conn);

    let ws = Workspace::open(&path).unwrap();
    let who = touched(&ws, &doc_id);
    assert_eq!(who.len(), 3, "replay attributed every block");
    assert_eq!(
        who.values().filter(|w| *w == "human:editor").count(),
        1,
        "replay kept the human's block apart from the agent's"
    );
}

/// Two agents, as in `acceptance.rs` — attribution must survive concurrency,
/// not just alternating turns.
#[test]
fn two_agents_keep_their_own_blocks() {
    let ws = Workspace::open_in_memory().unwrap();
    let (a, b) = (agent("opus"), agent("haiku"));

    let doc = ws.create_document("", &a).unwrap();
    ws.replace_block(&doc.doc_id, &doc.blocks[0].block_id, "Opening.", None, &a)
        .unwrap();

    let first = ws
        .insert_blocks(&doc.doc_id, &Position::End, "From opus.", None, &a)
        .unwrap()
        .block_id
        .unwrap();
    let second = ws
        .insert_blocks(&doc.doc_id, &Position::End, "From haiku.", None, &b)
        .unwrap()
        .block_id
        .unwrap();

    let who = touched(&ws, &doc.doc_id);
    assert_eq!(who[&first], "agent:opus");
    assert_eq!(who[&second], "agent:haiku");
}
