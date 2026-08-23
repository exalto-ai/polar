//! The CRDT encoding must be lossless for *every* schema-valid document —
//! not merely for the subset markdown can express.
//!
//! This uses `thought_testkit::document`, the unconstrained generator, precisely
//! because the CRDT is the source of truth (AD-3). If this ever has to fall
//! back to the markdown-safe generator, the storage format has acquired
//! markdown's limitations and the argument for a structured tree has collapsed.

use proptest::prelude::*;
use thought_core::{Document, Position};
use thought_schema::{Mark, Node, Schema, normalize};
use thought_testkit::document;

fn through_crdt(doc: &Node) -> Node {
    let crdt = Document::new();
    crdt.set_document(doc);
    normalize(&crdt.read())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn crdt_encoding_is_lossless(doc in document()) {
        let expected = normalize(&doc);
        prop_assert_eq!(through_crdt(&expected), expected);
    }

    #[test]
    fn crdt_output_is_schema_valid(doc in document()) {
        let actual = through_crdt(&normalize(&doc));
        if let Err(errs) = Schema::v0().validate(&actual) {
            let listed: Vec<String> = errs.iter().map(ToString::to_string).collect();
            prop_assert!(false, "CRDT produced an invalid document:\n  {}", listed.join("\n  "));
        }
    }

    /// Two replicas that exchange updates must agree, and must agree on the
    /// same document the origin holds.
    #[test]
    fn replicas_converge(doc in document()) {
        let expected = normalize(&doc);

        let a = Document::new();
        a.set_document(&expected);

        let b = Document::new();
        b.apply_update(&a.encode_state()).unwrap();

        prop_assert_eq!(normalize(&b.read()), expected.clone());
        prop_assert_eq!(b.state_vector(), a.state_vector());
    }
}

/// The shapes markdown cannot hold but the CRDT must: this is the concrete
/// payoff of AD-3, so it is asserted rather than assumed.
#[test]
fn crdt_holds_what_markdown_cannot() {
    let cases = vec![
        (
            "empty paragraph",
            Node::element("doc", vec![Node::element("paragraph", vec![])]),
        ),
        (
            "consecutive empty paragraphs",
            Node::element(
                "doc",
                vec![
                    Node::element("paragraph", vec![]),
                    Node::element("paragraph", vec![]),
                ],
            ),
        ),
        (
            "marked span fused to a word",
            Node::element(
                "doc",
                vec![Node::element(
                    "paragraph",
                    vec![
                        Node::text("A", vec![]),
                        Node::text("*", vec![Mark::new("bold")]),
                    ],
                )],
            ),
        ),
    ];

    for (label, doc) in cases {
        let expected = normalize(&doc);
        assert_eq!(through_crdt(&expected), expected, "CRDT lost: {label}");
    }
}

/// Concurrent fine-grained edits must both survive. This is the claim the whole
/// architecture rests on, so it is tested directly rather than inferred from
/// the fact that two replicas ended up equal.
#[test]
fn concurrent_edits_to_different_blocks_both_survive() {
    let base = normalize(&Node::element(
        "doc",
        vec![
            Node::element("paragraph", vec![Node::text("first", vec![])]),
            Node::element("paragraph", vec![Node::text("second", vec![])]),
        ],
    ));

    let a = Document::new();
    a.set_document(&base);
    let b = Document::new();
    b.apply_update(&a.encode_state()).unwrap();

    let ids: Vec<String> = a.blocks().into_iter().map(|b| b.block_id).collect();
    // Both replicas must agree on block identity, or cross-machine anchors are
    // meaningless.
    assert_eq!(
        ids,
        b.blocks()
            .into_iter()
            .map(|b| b.block_id)
            .collect::<Vec<_>>()
    );

    let (sv_a, sv_b) = (a.state_vector(), b.state_vector());
    a.append_text(&ids[0], " (A)").unwrap();
    b.append_text(&ids[1], " (B)").unwrap();

    // Exchange only what each side is missing.
    let from_a = a.diff_since(&sv_b);
    let from_b = b.diff_since(&sv_a);
    b.apply_update(&from_a).unwrap();
    a.apply_update(&from_b).unwrap();

    assert_eq!(
        normalize(&a.read()),
        normalize(&b.read()),
        "replicas diverged"
    );
    assert_eq!(a.block_text(&ids[0]).unwrap(), "first (A)");
    assert_eq!(a.block_text(&ids[1]).unwrap(), "second (B)");
}

/// Two actors appending to the *same* block. Neither edit may be lost — this is
/// precisely what a whole-document write would destroy, and why AD-5 bans it.
#[test]
fn concurrent_edits_to_the_same_block_both_survive() {
    let base = normalize(&Node::element(
        "doc",
        vec![Node::element("paragraph", vec![Node::text("base", vec![])])],
    ));

    let a = Document::new();
    a.set_document(&base);
    let b = Document::new();
    b.apply_update(&a.encode_state()).unwrap();

    let id = a.blocks()[0].block_id.clone();
    let (sv_a, sv_b) = (a.state_vector(), b.state_vector());

    a.append_text(&id, "-A").unwrap();
    b.append_text(&id, "-B").unwrap();

    let from_a = a.diff_since(&sv_b);
    let from_b = b.diff_since(&sv_a);
    b.apply_update(&from_a).unwrap();
    a.apply_update(&from_b).unwrap();

    assert_eq!(
        normalize(&a.read()),
        normalize(&b.read()),
        "replicas diverged"
    );
    let merged = a.block_text(&id).unwrap();
    assert!(
        merged.contains("-A") && merged.contains("-B"),
        "lost an edit: {merged}"
    );
    assert!(
        merged.starts_with("base"),
        "corrupted the original: {merged}"
    );
}

/// Identity survives an in-place edit and only changes when it must — the
/// contract M1.3 states, asserted rather than assumed.
#[test]
fn block_identity_is_stable_across_edits() {
    let doc = Document::new();
    doc.set_document(&normalize(&Node::element(
        "doc",
        vec![Node::element("paragraph", vec![Node::text("one", vec![])])],
    )));

    let id = doc.blocks()[0].block_id.clone();

    // Same node type: edited in place, so anchors held elsewhere stay valid.
    let replaced = doc
        .replace_block(
            &id,
            &Node::element("paragraph", vec![Node::text("rewritten", vec![])]),
        )
        .unwrap();
    assert_eq!(
        replaced.block_id, id,
        "same-type replace must preserve identity"
    );
    assert_eq!(doc.block_text(&id).unwrap(), "rewritten");

    // Changing type cannot preserve identity: an XmlElement's tag is immutable.
    // The caller is told the new id rather than left to discover it.
    let promoted = doc
        .replace_block(
            &id,
            &Node::element("heading", vec![Node::text("rewritten", vec![])])
                .with_attr("level", 2.into()),
        )
        .unwrap();
    assert_ne!(promoted.block_id, id, "type change mints a new identity");
    assert_eq!(doc.blocks()[0].kind, "heading");
    assert_eq!(doc.read().content[0].attrs["level"], 2);
}

#[test]
fn insert_and_delete_address_blocks_by_id() {
    let doc = Document::new();
    doc.set_document(&normalize(&Node::element(
        "doc",
        vec![Node::element("paragraph", vec![Node::text("a", vec![])])],
    )));
    let first = doc.blocks()[0].block_id.clone();

    doc.insert_blocks(
        &Position::After(first.clone()),
        &[Node::element("paragraph", vec![Node::text("b", vec![])])],
    )
    .unwrap();
    doc.insert_blocks(
        &Position::Start,
        &[Node::element("paragraph", vec![Node::text("z", vec![])])],
    )
    .unwrap();

    let texts: Vec<String> = doc
        .blocks()
        .iter()
        .map(|b| doc.block_text(&b.block_id).unwrap())
        .collect();
    assert_eq!(texts, vec!["z", "a", "b"]);

    doc.delete_block(&first).unwrap();
    let texts: Vec<String> = doc
        .blocks()
        .iter()
        .map(|b| doc.block_text(&b.block_id).unwrap())
        .collect();
    assert_eq!(texts, vec!["z", "b"]);

    // A vanished block reports news, not a crash (M1.6).
    assert!(matches!(
        doc.delete_block(&first),
        Err(thought_core::BlockError::NoSuchBlock(_))
    ));
}
