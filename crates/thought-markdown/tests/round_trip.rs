//! M1.0 acceptance criterion 3: `parse(serialize(doc)) == doc`.
//!
//! Generators live in `thought-testkit`, which distinguishes documents that are
//! merely schema-valid from those CommonMark can also express. This file uses
//! the markdown-safe variant; the gap between the two *is* the set of
//! limitations recorded in AD-12 and pinned below.

use proptest::prelude::*;
use thought_markdown::{from_markdown, normalize, round_trip, to_markdown};
use thought_schema::{Mark, Node, Schema};
use thought_testkit::markdown_safe_document as document;

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

    /// Closes the loop that bit twice in step 1: when the generator emits a tree
    /// the schema forbids, the failure must name the generator rather than
    /// arriving disguised as a broken serializer.
    #[test]
    fn generated_documents_are_schema_valid(doc in document()) {
        let doc = normalize(&doc);
        if let Err(errs) = Schema::v0().validate(&doc) {
            let listed: Vec<String> = errs.iter().map(ToString::to_string).collect();
            prop_assert!(false, "generator emitted an invalid document:\n  {}", listed.join("\n  "));
        }
    }

    /// And the parser must not manufacture invalid trees out of valid markdown —
    /// the tight-list bug produced exactly that.
    #[test]
    fn parsed_documents_are_schema_valid(doc in document()) {
        let parsed = round_trip(&normalize(&doc));
        if let Err(errs) = Schema::v0().validate(&parsed) {
            let listed: Vec<String> = errs.iter().map(ToString::to_string).collect();
            prop_assert!(false, "parser produced an invalid document:\n  {}", listed.join("\n  "));
        }
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
                vec![Node::text("bold", vec![Mark::new("bold")])],
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

#[test]
fn font_size_round_trips_through_a_stable_html_span() {
    let sized = Node::element(
        "doc",
        vec![Node::element(
            "paragraph",
            vec![Node::text(
                "Large",
                vec![Mark::new("fontSize").with_attr("size", "18px".into())],
            )],
        )],
    );
    let expected = normalize(&sized);

    assert_eq!(
        to_markdown(&expected),
        "<span style=\"font-size: 18px\">Large</span>"
    );
    assert_eq!(round_trip(&expected), expected);

    // Font sizing must compose with the CommonMark marks it sits beside.
    let composed = Node::element(
        "doc",
        vec![Node::element(
            "paragraph",
            vec![Node::text(
                "Large",
                vec![
                    Mark::new("bold"),
                    Mark::new("fontSize").with_attr("size", "18px".into()),
                ],
            )],
        )],
    );
    let expected = normalize(&composed);
    assert_eq!(round_trip(&expected), expected);
}

#[test]
fn title_and_h1_remain_distinct_in_the_projection() {
    let title = Node::element(
        "doc",
        vec![
            Node::element("heading", vec![Node::text("A title", vec![])])
                .with_attr("level", 1.into())
                .with_attr("variant", "title".into()),
        ],
    );
    let h1 = Node::element(
        "doc",
        vec![
            Node::element("heading", vec![Node::text("An H1", vec![])])
                .with_attr("level", 1.into()),
        ],
    );

    assert_eq!(to_markdown(&title), "<!--thought:title-->\n# A title");
    assert_eq!(to_markdown(&h1), "# An H1");
    assert_eq!(round_trip(&title), normalize(&title));
    assert_eq!(round_trip(&h1), normalize(&h1));

    // A marker only applies to the immediately following H1.
    let interrupted = normalize(&from_markdown("<!--thought:title-->\n\nParagraph\n\n# H1"));
    assert_eq!(interrupted.content[1].attr_str("variant"), None);

    let interrupted_by_rule = normalize(&from_markdown("<!--thought:title-->\n\n***\n\n# H1"));
    assert_eq!(interrupted_by_rule.content[1].attr_str("variant"), None);

    let escaped_blockquote = normalize(&from_markdown("> <!--thought:title-->\n\n# H1"));
    assert_eq!(escaped_blockquote.content[1].attr_str("variant"), None);
}

#[test]
fn link_destinations_and_titles_escape_without_changing_the_mark() {
    let link = Mark::new("link")
        .with_attr(
            "href",
            "https://example.com/a path_(v1)?q=<quoted>\\tail&literal=&copy;".into(),
        )
        .with_attr(
            "title",
            "Say \"hello\" from \\ here & keep &copy; literal".into(),
        );
    let document = Node::element(
        "doc",
        vec![Node::element(
            "paragraph",
            vec![Node::text("Open it", vec![link])],
        )],
    );

    assert_eq!(round_trip(&document), normalize(&document));
}

#[test]
fn font_size_parser_rejects_unsafe_css_and_respects_span_nesting() {
    for markdown in [
        "<span style=\"font-size: 7px\">plain</span>",
        "<span style=\"font-size: 97px\">plain</span>",
        "<span style=\"font-size: 18.5px\">plain</span>",
        "<span style=\"font-size: 1rem\">plain</span>",
        "<span style=\"font-size: 18px; color: red\">plain</span>",
    ] {
        let parsed = normalize(&from_markdown(markdown));
        assert!(
            parsed.content[0].content[0].marks.is_empty(),
            "unsafe span produced a mark: {markdown}"
        );
    }

    // The ignored inner span must not close the recognized outer span.
    let nested = normalize(&from_markdown(
        "<span style=\"font-size: 18px\">a<span class=\"note\">b</span>c</span>",
    ));
    assert_eq!(nested.content[0].content.len(), 1);
    assert_eq!(nested.content[0].content[0].text.as_deref(), Some("abc"));
    assert_eq!(
        nested.content[0].content[0].marks,
        vec![Mark::new("fontSize").with_attr("size", "18px".into())]
    );

    // A recognized inner size overrides its parent without corrupting the
    // surrounding CommonMark stack, then restores the parent on close.
    let nested_sizes = normalize(&from_markdown(
        "<span style=\"font-size: 18px\">**outer <span style=\"font-size: 20px\">inner</span> outer**</span>",
    ));
    let runs = &nested_sizes.content[0].content;
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[0].text.as_deref(), Some("outer "));
    assert_eq!(runs[1].text.as_deref(), Some("inner"));
    assert_eq!(runs[2].text.as_deref(), Some(" outer"));
    assert!(
        runs.iter()
            .all(|run| run.marks.iter().any(|mark| mark.kind == "bold"))
    );
    assert_eq!(runs[0].marks[1].attrs["size"], "18px");
    assert_eq!(runs[1].marks[1].attrs["size"], "20px");
    assert_eq!(runs[2].marks[1].attrs["size"], "18px");

    // A malformed unclosed span is contained to its block.
    let unclosed = normalize(&from_markdown(
        "<span style=\"font-size: 18px\">first\n\nsecond",
    ));
    assert_eq!(unclosed.content.len(), 2);
    assert!(
        unclosed.content[0].content[0]
            .marks
            .iter()
            .any(|mark| mark.kind == "fontSize")
    );
    assert!(unclosed.content[1].content[0].marks.is_empty());
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
    assert!(!has(r"A**\***", "bold"), "fused, content is `*`");
    assert!(!has(r"A**\*x**", "bold"), "fused, content starts with `*`");
    assert!(!has(r"A**\_**", "bold"), "fused, content is `_`");
    assert!(!has(r"A*\**", "italic"), "fused em, content is `*`");
    assert!(!has(r"A**\`**", "bold"), "fused, content is a backtick");
    // ...or by a nested mark's own delimiter, which is the broader case.
    assert!(
        !has(r"A**~~a~~**", "bold"),
        "nested mark supplies the punctuation"
    );
    assert!(
        has(r"A**~~a~~**", "strike"),
        "and the inner mark still survives"
    );
    // The rule is symmetric — the closing side fails the same way.
    assert!(
        !has(r"**~~A~~**0", "bold"),
        "closing delimiter fused to next word"
    );

    // Whitespace on the relevant side resolves every one of them.
    assert!(
        has(r"A **\*** B", "bold"),
        "whitespace separates the delimiter"
    );
    assert!(
        has(r"A **~~a~~** B", "bold"),
        "whitespace fixes the nested case"
    );
    assert!(has(r"**\***", "bold"), "span stands alone");
    assert!(has(r"A**x\***", "bold"), "content merely ends with `*`");
    assert!(has(r"A**x\*x**", "bold"), "punctuation is interior");
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

    // The first row is always the header — GFM has no headerless table — and
    // header cells are their own node type, matching TipTap.
    let t = from_markdown("| h |\n| --- |\n| b |\n");
    assert_eq!(t.content[0].content[0].content[0].kind, "tableHeader");
    assert_eq!(t.content[0].content[1].content[0].kind, "tableCell");
    // And the cell wraps its text in a block, as ProseMirror requires.
    assert_eq!(
        t.content[0].content[0].content[0].content[0].kind,
        "paragraph"
    );

    // A cell holds blocks, not inline content — ProseMirror's shape, which
    // TipTap's table extension defines and Rust follows (M2.2). A literal pipe
    // inside one has to be escaped or it re-parses as a column break.
    let with_pipe = Node::element(
        "doc",
        vec![Node::element(
            "table",
            vec![Node::element(
                "tableRow",
                vec![Node::element(
                    "tableHeader",
                    vec![Node::element("paragraph", vec![Node::text("a|b", vec![])])],
                )],
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

/// Line spans must actually address the block they claim to, or every agent
/// edit lands somewhere else.
#[test]
fn line_spans_locate_their_blocks() {
    let doc = Node::element(
        "doc",
        vec![
            Node::element("heading", vec![Node::text("Title", vec![])])
                .with_attr("level", 1.into()),
            Node::element("paragraph", vec![Node::text("first para", vec![])]),
            Node::element(
                "bulletList",
                vec![
                    Node::element(
                        "listItem",
                        vec![Node::element("paragraph", vec![Node::text("one", vec![])])],
                    ),
                    Node::element(
                        "listItem",
                        vec![Node::element("paragraph", vec![Node::text("two", vec![])])],
                    ),
                ],
            ),
        ],
    );

    let (md, spans) = thought_markdown::to_markdown_with_spans(&doc);
    let lines: Vec<&str> = md.lines().collect();

    assert_eq!(
        spans.len(),
        doc.content.len(),
        "one span per top-level block"
    );
    assert!(lines[spans[0].0 - 1].starts_with("# Title"));
    assert!(lines[spans[1].0 - 1].starts_with("first para"));

    // A multi-line block must span all of its lines, not just the first.
    let (start, end) = spans[2];
    assert_eq!(end - start, 1, "the two-item list occupies two lines");
    assert!(lines[start - 1].contains("one"));
    assert!(lines[end - 1].contains("two"));
}

/// Every span has to point at a line that exists.
///
/// A block can serialize to nothing — an empty paragraph does — and the naive
/// computation gave it the line *after* the document, so an agent asking for
/// line 12 of an 11-line document got an answer rather than an error.
#[test]
fn line_spans_stay_inside_the_document() {
    let doc = Node::element(
        "doc",
        vec![
            Node::element("heading", vec![Node::text("Title", vec![])])
                .with_attr("level", 1.into()),
            Node::element("paragraph", vec![Node::text("Body.", vec![])]),
            // Renders to nothing at all.
            Node::element("paragraph", vec![]),
        ],
    );

    let (markdown, spans) = thought_markdown::to_markdown_with_spans(&doc);
    let lines = markdown.lines().count();

    assert_eq!(spans.len(), doc.content.len());
    for (i, (start, end)) in spans.iter().enumerate() {
        assert!(*start >= 1, "block {i} starts at line {start}");
        assert!(
            *end <= lines,
            "block {i} ends at line {end}, past the {lines} the document has"
        );
        assert!(start <= end, "block {i} spans backwards");
    }
}
