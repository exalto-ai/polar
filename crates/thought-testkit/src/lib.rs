//! Document generators shared by every crate that needs to prove a
//! representation is lossless.
//!
//! Two generators, and the difference between them is the point:
//! [`document`] emits any schema-valid document, while
//! [`markdown_safe_document`] additionally avoids the shapes CommonMark cannot
//! express. Keeping them apart stops markdown's limitations from silently
//! becoming constraints on representations that do not share them — the CRDT
//! encoding, for instance, can hold everything the schema allows.

use proptest::prelude::*;
use thought_schema::{Mark, Node};

/// Text leaning on the characters that break naive markdown escaping.
pub fn text() -> impl Strategy<Value = String> {
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
            1 => Just("|".to_string()),
            2 => Just(" ".to_string()),
        ],
        1..6,
    )
    // Markdown strips leading and trailing whitespace from a block, and the
    // CRDT has no reason to disagree, so both generators trim.
    .prop_map(|parts| parts.concat().trim().to_string())
    .prop_filter("non-empty after trim", |s| !s.is_empty())
}

pub fn marks() -> impl Strategy<Value = Vec<Mark>> {
    prop::collection::vec(
        prop_oneof![
            Just(Mark::new("bold")),
            Just(Mark::new("italic")),
            Just(Mark::new("strike")),
            Just(Mark::new("code")),
            Just(Mark::new("link").with_attr("href", "https://example.com".into())),
            Just(Mark::new("fontSize").with_attr("size", "18px".into())),
        ],
        0..3,
    )
}

fn inlines(markdown_safe: bool) -> BoxedStrategy<Vec<Node>> {
    prop::collection::vec((text(), marks()), 1..4)
        .prop_map(move |parts| {
            let mut out: Vec<Node> = Vec::new();
            let mut after_marked = false;
            for (t, m) in parts {
                let marked = !m.is_empty();
                // A marked span fused to adjacent word characters cannot
                // round-trip through CommonMark: the flanking rules refuse to
                // open or close emphasis when punctuation sits against the
                // delimiter, and every mark delimiter is punctuation. Only the
                // markdown-safe generator pays for this.
                if markdown_safe && (marked || after_marked) && !out.is_empty() {
                    out.push(Node::text(" ", vec![]));
                }
                out.push(Node::text(t, m));
                after_marked = marked;
            }
            out
        })
        .boxed()
}

fn table(markdown_safe: bool) -> BoxedStrategy<Node> {
    // Rectangular by construction: prosemirror-tables enforces it, and GFM pads
    // every row to the header width, so a ragged table is a document no editor
    // could hand us.
    (1usize..4, 1usize..4)
        .prop_flat_map(move |(rows, cols)| {
            prop::collection::vec(inlines(markdown_safe), rows * cols)
                .prop_map(move |cells| (rows, cols, cells))
        })
        .prop_map(|(rows, cols, cells)| {
            let rows: Vec<Node> = (0..rows)
                .map(|r| {
                    // GFM has no headerless table, so the first row is always
                    // header cells — a distinct node type, as TipTap expects.
                    let kind = if r == 0 { "tableHeader" } else { "tableCell" };
                    let row_cells = (0..cols)
                        .map(|c| {
                            // Cells hold blocks, not inline content.
                            let para = Node::element("paragraph", cells[r * cols + c].clone());
                            Node::element(kind, vec![para])
                        })
                        .collect();
                    Node::element("tableRow", row_cells)
                })
                .collect();
            Node::element("table", rows)
        })
        .boxed()
}

fn block(markdown_safe: bool) -> BoxedStrategy<Node> {
    let leaf = prop_oneof![
        inlines(markdown_safe).prop_map(|c| Node::element("paragraph", c)),
        (1i64..=3, any::<bool>(), inlines(markdown_safe)).prop_map(|(l, title, c)| {
            let heading = Node::element("heading", c).with_attr("level", l.into());
            if l == 1 && title {
                heading.with_attr("variant", "title".into())
            } else {
                heading
            }
        }),
        Just(Node::element("horizontalRule", vec![])),
        ("[a-z ;=(){}\n]{1,30}", prop::option::of("[a-z]{1,6}")).prop_map(|(code, lang)| {
            let mut n = Node::element("codeBlock", vec![Node::text(code, vec![])]);
            if let Some(l) = lang {
                n = n.with_attr("language", l.into());
            }
            n
        }),
    ];
    let leaf = prop_oneof![4 => leaf, 1 => table(markdown_safe)];

    leaf.prop_recursive(3, 12, 3, move |inner| {
        // schema.json says listItem content is `paragraph block*`.
        let item = (inlines(markdown_safe), prop::option::of(inner.clone()))
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
    .boxed()
}

/// Any schema-valid document. Use this for representations that can hold
/// everything the schema allows.
pub fn document() -> BoxedStrategy<Node> {
    prop::collection::vec(block(false), 1..5)
        .prop_map(|c| Node::element("doc", c))
        .boxed()
}

/// Schema-valid documents that also avoid what CommonMark cannot express.
///
/// Note what is *not* excluded: empty blocks are unrepresentable in markdown
/// too, but neither generator emits them, so the gap is documented in AD-12
/// rather than encoded here.
pub fn markdown_safe_document() -> BoxedStrategy<Node> {
    prop::collection::vec(block(true), 1..5)
        .prop_map(|c| Node::element("doc", c))
        .boxed()
}
