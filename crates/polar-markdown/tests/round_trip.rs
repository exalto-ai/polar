//! M1.0 acceptance criterion 3: `parse(serialize(doc)) == doc`.
//!
//! The generator is deliberately adversarial about text content — markdown
//! metacharacters are where escaping bugs live, and a generator that only emits
//! alphanumerics would pass while the projection is broken.
//!
//! Where the generator is *constrained*, that constraint is a documented
//! limitation of markdown itself, not a convenience. Each one is called out.

use polar_markdown::{from_markdown, normalize, round_trip, to_markdown};
use polar_schema::{Mark, Node};
use proptest::prelude::*;

/// Text that leans on the characters that break naive escaping.
fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            8 => "[a-zA-Z0-9]{1,6}",
            1 => Just("*".to_string()),
            1 => Just("_".to_string()),
            1 => Just("`".to_string()),
            1 => Just("#".to_string()),
            1 => Just("[".to_string()),
            1 => Just("]".to_string()),
            1 => Just("~".to_string()),
            1 => Just("\\".to_string()),
            1 => Just(">".to_string()),
            1 => Just("-".to_string()),
            2 => Just(" ".to_string()),
        ],
        1..6,
    )
    // LIMITATION: markdown strips leading and trailing whitespace from a
    // paragraph, so text cannot begin or end with a space and survive.
    .prop_map(|parts| parts.concat().trim().to_string())
    .prop_filter("non-empty after trim", |s| !s.is_empty())
}

fn marks() -> impl Strategy<Value = Vec<Mark>> {
    prop::collection::vec(
        prop_oneof![
            Just(Mark::new("strong")),
            Just(Mark::new("em")),
            Just(Mark::new("strike")),
            Just(Mark::new("code")),
            Just(Mark::new("link").with_attr("href", "https://example.com".into())),
        ],
        0..3,
    )
}

fn inlines() -> impl Strategy<Value = Vec<Node>> {
    // LIMITATION (pinned in `intraword_emphasis_gap_is_pinned`): a marked span
    // fused directly to a preceding word cannot round-trip. CommonMark's
    // left-flanking rule refuses to open emphasis when the delimiter is
    // preceded by an alphanumeric and followed by punctuation — and the
    // following character is punctuation whenever the span's text starts with
    // any, OR whenever a second mark nests inside it, since every delimiter is
    // punctuation. Escaping cannot help: the backslash is punctuation too.
    // The right-flanking rule is the mirror image, so a span ending in
    // punctuation cannot be fused to the word *after* it either.
    //
    // Whitespace separation resolves every variant, so the generator inserts it
    // rather than excluding marks. This matches how people actually write —
    // emphasis is nearly always preceded by a space or starts the block.
    //
    // An empty paragraph is also unrepresentable, which is why AD-5 bans
    // whole-document replacement: an agent round-tripping a document through
    // markdown would silently delete every empty block. Surgical block edits
    // never see the whole tree, so the loss cannot propagate back.
    prop::collection::vec((text(), marks()), 1..4).prop_map(|parts| {
        let mut out: Vec<Node> = Vec::new();
        let mut after_marked = false;
        for (t, m) in parts {
            let marked = !m.is_empty();
            if (marked || after_marked) && !out.is_empty() {
                out.push(Node::text(" ", vec![]));
            }
            out.push(Node::text(t, m));
            after_marked = marked;
        }
        out
    })
}

fn block() -> impl Strategy<Value = Node> {
    let leaf = prop_oneof![
        inlines().prop_map(|c| Node::element("paragraph", c)),
        (1i64..=3, inlines())
            .prop_map(|(l, c)| Node::element("heading", c).with_attr("level", l.into())),
        Just(Node::element("horizontalRule", vec![])),
        ("[a-z ;=(){}\n]{1,30}", prop::option::of("[a-z]{1,6}")).prop_map(|(code, lang)| {
            let mut n = Node::element("codeBlock", vec![Node::text(code, vec![])]);
            if let Some(l) = lang {
                n = n.with_attr("language", l.into());
            }
            n
        }),
    ];

    // Rectangular by construction. GFM pads every row to the header's column
    // count, so a ragged table cannot round-trip — but ProseMirror tables are
    // rectangular anyway (prosemirror-tables enforces it), so a ragged table is
    // not a document we could ever be handed. Generating them tested a shape
    // that does not exist and blamed the serializer for the result.
    let table = (1usize..4, 1usize..4)
        .prop_flat_map(|(rows, cols)| {
            prop::collection::vec(inlines(), rows * cols).prop_map(move |cells| (rows, cols, cells))
        })
        .prop_map(|(rows, cols, cells)| {
            let rows: Vec<Node> = (0..rows)
                .map(|r| {
                    let row_cells = (0..cols)
                        .map(|c| {
                            Node::element("tableCell", cells[r * cols + c].clone())
                                .with_attr("header", (r == 0).into())
                        })
                        .collect();
                    Node::element("tableRow", row_cells)
                })
                .collect();
            Node::element("table", rows)
        });

    let leaf = prop_oneof![4 => leaf, 1 => table];

    leaf.prop_recursive(3, 12, 3, |inner| {
        // schema.json says listItem content is `paragraph block*` — the first
        // child must be a paragraph. Generating a bare block here produced
        // schema-invalid documents and blamed the serializer for the result.
        let item = (inlines(), prop::option::of(inner.clone()))
            .prop_map(|(inl, extra)| {
                let mut content = vec![Node::element("paragraph", inl)];
                if let Some(block) = extra {
                    content.push(block);
                }
                Node::element("listItem", content)
            })
            .boxed();

        prop_oneof![
            prop::collection::vec(inner, 1..3).prop_map(|c| Node::element("blockquote", c)),
            prop::collection::vec(item.clone(), 1..3)
                .prop_map(|items| Node::element("bulletList", items)),
            prop::collection::vec(item, 1..3).prop_map(|items| {
                Node::element("orderedList", items).with_attr("start", 1.into())
            }),
        ]
    })
}

fn document() -> impl Strategy<Value = Node> {
    prop::collection::vec(block(), 1..5).prop_map(|c| Node::element("doc", c))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn parse_of_serialize_is_identity(doc in document()) {
        let expected = normalize(&doc);
        let actual = round_trip(&expected);
        prop_assert_eq!(
            &actual, &expected,
            "\n--- markdown ---\n{}\n--- expected ---\n{}\n--- actual ---\n{}\n",
            to_markdown(&expected),
            serde_json::to_string_pretty(&expected).unwrap(),
            serde_json::to_string_pretty(&actual).unwrap()
        );
    }

    /// Serializing is deterministic and stable under repetition — a projection
    /// that drifts on each pass would make every agent read look like an edit.
    #[test]
    fn serialize_is_idempotent(doc in document()) {
        let once = to_markdown(&normalize(&doc));
        let twice = to_markdown(&normalize(&from_markdown(&once)));
        prop_assert_eq!(once, twice);
    }
}

#[test]
fn known_shapes_round_trip() {
    let cases = vec![
        Node::element(
            "doc",
            vec![Node::element(
                "paragraph",
                vec![Node::text("plain", vec![])],
            )],
        ),
        Node::element(
            "doc",
            vec![Node::element(
                "paragraph",
                vec![Node::text("bold", vec![Mark::new("strong")])],
            )],
        ),
        Node::element(
            "doc",
            vec![Node::element(
                "paragraph",
                vec![Node::text("2 * 3 * 4", vec![])],
            )],
        ),
        Node::element(
            "doc",
            vec![Node::element(
                "codeBlock",
                vec![Node::text("let x = 1;", vec![])],
            )],
        ),
    ];
    for case in cases {
        let expected = normalize(&case);
        assert_eq!(
            round_trip(&expected),
            expected,
            "\nmd:\n{}",
            to_markdown(&expected)
        );
    }
}

/// Pins the exact boundary of the one shape markdown cannot represent, so a
/// pulldown-cmark upgrade that widens or narrows it is noticed rather than
/// silently absorbed. `true` means the mark survives a round trip.
#[test]
fn intraword_emphasis_gap_is_pinned() {
    // Must name the mark: in `A**~~a~~**` the strike survives while the strong
    // does not, so asking merely whether *any* mark survived reports a pass.
    let has = |md: &str, kind: &str| {
        serde_json::to_string(&from_markdown(md))
            .unwrap()
            .contains(&format!("{{\"type\":\"{kind}\"}}"))
    };

    // The gap: a marked span fused to adjacent word characters, where what sits
    // against the delimiter is punctuation — supplied by the text itself...
    assert!(!has(r"A**\***", "strong"), "fused, content is `*`");
    assert!(
        !has(r"A**\*x**", "strong"),
        "fused, content starts with `*`"
    );
    assert!(!has(r"A**\_**", "strong"), "fused, content is `_`");
    assert!(!has(r"A*\**", "em"), "fused em, content is `*`");
    assert!(!has(r"A**\`**", "strong"), "fused, content is a backtick");
    // ...or by a nested mark's own delimiter, which is the broader case.
    assert!(
        !has(r"A**~~a~~**", "strong"),
        "nested mark supplies the punctuation"
    );
    assert!(
        has(r"A**~~a~~**", "strike"),
        "and the inner mark still survives"
    );
    // The rule is symmetric — the closing side fails the same way.
    assert!(
        !has(r"**~~A~~**0", "strong"),
        "closing delimiter fused to next word"
    );

    // Whitespace on the relevant side resolves every one of them.
    assert!(
        has(r"A **\*** B", "strong"),
        "whitespace separates the delimiter"
    );
    assert!(
        has(r"A **~~a~~** B", "strong"),
        "whitespace fixes the nested case"
    );
    assert!(has(r"**\***", "strong"), "span stands alone");
    assert!(has(r"A**x\***", "strong"), "content merely ends with `*`");
    assert!(has(r"A**x\*x**", "strong"), "punctuation is interior");
    assert!(
        has(r"A~~\~~~", "strike"),
        "strike opens where emphasis would not"
    );
}

/// Tables survive v0 (resolving M1.8 open question 3), but only under two
/// constraints that are worth stating rather than discovering later.
#[test]
fn table_constraints_are_pinned() {
    // GFM pads every row to the header's column count, so a ragged table comes
    // back rectangular. ProseMirror tables are rectangular anyway, so this
    // constrains what we may *emit*, not what we can represent.
    let ragged = from_markdown("| a | b |\n| --- | --- |\n| c |\n");
    let row = &ragged.content[0].content[1];
    assert_eq!(row.content.len(), 2, "body row padded to header width");

    // The first row is always the header — GFM has no headerless table.
    let t = from_markdown("| h |\n| --- |\n| b |\n");
    assert_eq!(t.content[0].content[0].content[0].attrs["header"], true);
    assert_eq!(t.content[0].content[1].content[0].attrs["header"], false);

    // Cells hold inline content only; schema.json says `inline*`, and GFM
    // cannot express a block inside a cell regardless.
    let with_pipe = Node::element(
        "doc",
        vec![Node::element(
            "table",
            vec![Node::element(
                "tableRow",
                vec![
                    Node::element("tableCell", vec![Node::text("a|b", vec![])])
                        .with_attr("header", true.into()),
                ],
            )],
        )],
    );
    let expected = normalize(&with_pipe);
    assert_eq!(
        round_trip(&expected),
        expected,
        "a literal pipe must be escaped"
    );
}
