//! Adapter from the live CRDT/tree representation to semantic-lineage input.
//!
//! The provenance engine deliberately knows nothing about Yjs or the
//! ProseMirror schema. This module is the narrow boundary: it pairs normalized
//! top-level nodes with their intrinsic CRDT block ids, records text-leaf
//! paths, and reduces rich structure and marks to deterministic canonical keys.

use serde::Serialize;
use std::fmt;
use thought_core::Document;
use thought_provenance::{BlockSnapshot, SemanticRangeAnchor, TextLeafSnapshot};
use thought_schema::{Attrs, Mark, Node};
use unicode_segmentation::UnicodeSegmentation;

/// One range reported by a complete editor dispatch.
///
/// Positions use ProseMirror's document coordinate space. Before positions
/// address the dispatch's input document and after positions address its
/// resulting document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProseMirrorRangeHint {
    pub before_from: u32,
    pub before_to: u32,
    pub after_from: u32,
    pub after_to: u32,
}

/// Validate ProseMirror ranges against the exact before and after trees, then
/// convert them to the provenance engine's canonical grapheme coordinates.
///
/// A position inside a grapheme is rejected. Positions at structural
/// boundaries are valid and may map to an empty grapheme range.
pub fn semantic_range_anchors(
    before: &Node,
    after: &Node,
    hints: &[ProseMirrorRangeHint],
) -> Result<Vec<SemanticRangeAnchor>, SnapshotError> {
    let before_positions = PositionedDocument::new("before", before)?;
    let after_positions = PositionedDocument::new("after", after)?;
    hints
        .iter()
        .map(|hint| {
            Ok(SemanticRangeAnchor {
                before: before_positions.range(hint.before_from, hint.before_to)?,
                after: after_positions.range(hint.after_from, hint.after_to)?,
            })
        })
        .collect()
}

/// Convert a document and its normalized ProseMirror tree into deterministic
/// provenance snapshots.
///
/// The tree must be the normalized tree read from `document`. A mismatch is an
/// error rather than an opportunity to attach a true block id to the wrong
/// visible block.
pub fn block_snapshots(
    document: &Document,
    normalized_tree: &Node,
) -> Result<Vec<BlockSnapshot>, SnapshotError> {
    if normalized_tree.kind != "doc" {
        return Err(SnapshotError::NotDocument(normalized_tree.kind.clone()));
    }

    let block_refs = document.blocks();
    if block_refs.len() != normalized_tree.content.len() {
        return Err(SnapshotError::BlockCount {
            document: block_refs.len(),
            tree: normalized_tree.content.len(),
        });
    }

    block_refs
        .into_iter()
        .zip(&normalized_tree.content)
        .enumerate()
        .map(|(index, (block_ref, node))| {
            if block_ref.kind != node.kind {
                return Err(SnapshotError::BlockKind {
                    index,
                    document: block_ref.kind,
                    tree: node.kind.clone(),
                });
            }

            let mut leaves = Vec::new();
            collect_text_leaves(node, &mut Vec::new(), &mut leaves);
            Ok(BlockSnapshot::new(
                block_ref.block_id,
                node.kind.clone(),
                canonical_structure_key(node),
                leaves,
            ))
        })
        .collect()
}

/// Canonical key for a text leaf's marks.
///
/// Normalized trees already sort marks and their attributes are `BTreeMap`s.
/// Sorting once more here keeps the adapter deterministic when used in a test
/// or migration with an older, not-quite-normalized tree.
pub fn canonical_format_key(marks: &[Mark]) -> String {
    if marks.is_empty() {
        return String::new();
    }
    let mut marks = marks.to_vec();
    marks.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| canonical_attrs(&left.attrs).cmp(&canonical_attrs(&right.attrs)))
    });
    serde_json::to_string(&marks).expect("schema marks always serialize")
}

/// Canonical structure key excluding visible text and formatting marks.
///
/// Consecutive text leaves collapse to one placeholder, so adding a mark that
/// splits a text run is a formatting delta, not a structural delta. Node kinds,
/// attributes, non-text nesting, and the placement of text among other child
/// nodes remain in the key.
pub fn canonical_structure_key(node: &Node) -> String {
    serde_json::to_string(&StructureNode::from_node(node))
        .expect("schema structure always serializes")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    NotDocument(String),
    BlockCount {
        document: usize,
        tree: usize,
    },
    BlockKind {
        index: usize,
        document: String,
        tree: String,
    },
    InvalidPositionDocument {
        side: &'static str,
        kind: String,
    },
    PositionOverflow {
        side: &'static str,
    },
    InvalidPositionRange {
        side: &'static str,
        from: u32,
        to: u32,
        size: u32,
    },
    NonGraphemePosition {
        side: &'static str,
        position: u32,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDocument(kind) => write!(f, "expected a `doc` root, found `{kind}`"),
            Self::BlockCount { document, tree } => write!(
                f,
                "document has {document} top-level blocks but tree has {tree}"
            ),
            Self::BlockKind {
                index,
                document,
                tree,
            } => write!(
                f,
                "block {index} is `{document}` in the document but `{tree}` in the tree"
            ),
            Self::InvalidPositionDocument { side, kind } => {
                write!(f, "{side} anchor tree must be a document, found `{kind}`")
            }
            Self::PositionOverflow { side } => {
                write!(f, "{side} anchor tree exceeds the supported position range")
            }
            Self::InvalidPositionRange {
                side,
                from,
                to,
                size,
            } => write!(
                f,
                "{side} anchor range {from}..{to} is outside document positions 0..{size}"
            ),
            Self::NonGraphemePosition { side, position } => write!(
                f,
                "{side} anchor position {position} splits a Unicode grapheme"
            ),
        }
    }
}

impl std::error::Error for SnapshotError {}

#[derive(Debug, Clone, Copy)]
struct PositionedGrapheme {
    from: u32,
    to: u32,
}

struct PositionedDocument {
    side: &'static str,
    size: u32,
    graphemes: Vec<PositionedGrapheme>,
}

impl PositionedDocument {
    fn new(side: &'static str, document: &Node) -> Result<Self, SnapshotError> {
        if document.kind != "doc" {
            return Err(SnapshotError::InvalidPositionDocument {
                side,
                kind: document.kind.clone(),
            });
        }
        let mut graphemes = Vec::new();
        let mut cursor = 0_u32;
        for child in &document.content {
            collect_positioned_graphemes(side, child, cursor, &mut graphemes)?;
            cursor = cursor
                .checked_add(node_size(side, child)?)
                .ok_or(SnapshotError::PositionOverflow { side })?;
        }
        Ok(Self {
            side,
            size: cursor,
            graphemes,
        })
    }

    fn range(&self, from: u32, to: u32) -> Result<std::ops::Range<usize>, SnapshotError> {
        if from > to || to > self.size {
            return Err(SnapshotError::InvalidPositionRange {
                side: self.side,
                from,
                to,
                size: self.size,
            });
        }
        for position in [from, to] {
            if self
                .graphemes
                .iter()
                .any(|grapheme| grapheme.from < position && position < grapheme.to)
            {
                return Err(SnapshotError::NonGraphemePosition {
                    side: self.side,
                    position,
                });
            }
        }
        let start = self
            .graphemes
            .partition_point(|grapheme| grapheme.to <= from);
        let end = self
            .graphemes
            .partition_point(|grapheme| grapheme.from < to);
        Ok(start..end)
    }
}

fn node_size(side: &'static str, node: &Node) -> Result<u32, SnapshotError> {
    if node.is_text() {
        return u32::try_from(
            node.text
                .as_deref()
                .unwrap_or_default()
                .encode_utf16()
                .count(),
        )
        .map_err(|_| SnapshotError::PositionOverflow { side });
    }
    if node.kind == "horizontalRule" {
        return Ok(1);
    }
    let mut content_size = 0_u32;
    for child in &node.content {
        content_size = content_size
            .checked_add(node_size(side, child)?)
            .ok_or(SnapshotError::PositionOverflow { side })?;
    }
    content_size
        .checked_add(2)
        .ok_or(SnapshotError::PositionOverflow { side })
}

fn collect_positioned_graphemes(
    side: &'static str,
    node: &Node,
    node_start: u32,
    out: &mut Vec<PositionedGrapheme>,
) -> Result<(), SnapshotError> {
    if node.is_text() {
        let mut utf16 = 0_u32;
        for grapheme in node.text.as_deref().unwrap_or_default().graphemes(true) {
            let from = node_start
                .checked_add(utf16)
                .ok_or(SnapshotError::PositionOverflow { side })?;
            utf16 = utf16
                .checked_add(
                    u32::try_from(grapheme.encode_utf16().count())
                        .map_err(|_| SnapshotError::PositionOverflow { side })?,
                )
                .ok_or(SnapshotError::PositionOverflow { side })?;
            let to = node_start
                .checked_add(utf16)
                .ok_or(SnapshotError::PositionOverflow { side })?;
            out.push(PositionedGrapheme { from, to });
        }
        return Ok(());
    }
    if node.kind == "horizontalRule" {
        return Ok(());
    }
    let mut child_start = node_start
        .checked_add(1)
        .ok_or(SnapshotError::PositionOverflow { side })?;
    for child in &node.content {
        collect_positioned_graphemes(side, child, child_start, out)?;
        child_start = child_start
            .checked_add(node_size(side, child)?)
            .ok_or(SnapshotError::PositionOverflow { side })?;
    }
    Ok(())
}

fn collect_text_leaves(node: &Node, path: &mut Vec<u32>, out: &mut Vec<TextLeafSnapshot>) {
    for (index, child) in node.content.iter().enumerate() {
        path.push(index as u32);
        if child.is_text() {
            if let Some(text) = child.text.as_deref()
                && !text.is_empty()
            {
                out.push(TextLeafSnapshot::new(
                    path.clone(),
                    text,
                    canonical_format_key(&child.marks),
                ));
            }
        } else {
            collect_text_leaves(child, path, out);
        }
        path.pop();
    }
}

fn canonical_attrs(attrs: &Attrs) -> String {
    serde_json::to_string(attrs).expect("schema attributes always serialize")
}

#[derive(Serialize)]
struct StructureNode<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(skip_serializing_if = "Attrs::is_empty")]
    attrs: &'a Attrs,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    content: Vec<StructureNode<'a>>,
}

impl<'a> StructureNode<'a> {
    fn from_node(node: &'a Node) -> Self {
        let mut content = Vec::new();
        let mut previous_was_text = false;
        for child in &node.content {
            if child.is_text() {
                if !previous_was_text {
                    content.push(Self {
                        kind: "text",
                        attrs: &child.attrs,
                        content: vec![],
                    });
                }
                previous_was_text = true;
            } else {
                content.push(Self::from_node(child));
                previous_was_text = false;
            }
        }
        Self {
            kind: &node.kind,
            attrs: &node.attrs,
            content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use thought_schema::{Mark, normalize};

    fn snapshots(tree: Node) -> (Document, Node, Vec<BlockSnapshot>) {
        let tree = normalize(&tree);
        let document = Document::new();
        document.set_document(&tree);
        let snapshots = block_snapshots(&document, &tree).unwrap();
        (document, tree, snapshots)
    }

    #[test]
    fn plain_text_uses_the_crdt_block_id_and_child_path() {
        let tree = Node::element(
            "doc",
            vec![Node::element(
                "paragraph",
                vec![Node::text("Plain text", vec![])],
            )],
        );
        let (document, _, snapshots) = snapshots(tree);

        assert_eq!(snapshots[0].kind, "paragraph");
        assert_eq!(snapshots[0].leaves[0].path, vec![0]);
        assert_eq!(snapshots[0].leaves[0].text, "Plain text");
        assert_eq!(snapshots[0].block_id, document.blocks()[0].block_id);
    }

    #[test]
    fn nested_list_and_table_paths_and_structure_are_preserved() {
        let list = Node::element(
            "bullet_list",
            vec![Node::element(
                "list_item",
                vec![Node::element(
                    "paragraph",
                    vec![Node::text("List value", vec![])],
                )],
            )],
        );
        let table = Node::element(
            "table",
            vec![Node::element(
                "table_row",
                vec![Node::element(
                    "table_cell",
                    vec![Node::element(
                        "paragraph",
                        vec![Node::text("Cell value", vec![])],
                    )],
                )],
            )],
        );
        let (_, _, snapshots) = snapshots(Node::element("doc", vec![list, table]));

        assert_eq!(snapshots[0].leaves[0].path, vec![0, 0, 0]);
        assert_eq!(snapshots[1].leaves[0].path, vec![0, 0, 0, 0]);
        assert!(snapshots[0].structure_key.contains("bullet_list"));
        assert!(snapshots[1].structure_key.contains("table_cell"));
        assert!(!snapshots[0].structure_key.contains("List value"));
        assert!(!snapshots[1].structure_key.contains("Cell value"));
    }

    #[test]
    fn mark_keys_are_canonical_and_do_not_change_structure() {
        let bold = Mark::new("bold");
        let link = Mark::new("link").with_attr("href", json!("https://example.com"));
        let plain = Node::element("paragraph", vec![Node::text("same text", vec![])]);
        let marked = Node::element(
            "paragraph",
            vec![
                Node::text("same ", vec![bold.clone(), link.clone()]),
                Node::text("text", vec![link, bold]),
            ],
        );
        let (_, _, plain_snapshot) = snapshots(Node::element("doc", vec![plain]));
        let (_, _, marked_snapshot) = snapshots(Node::element("doc", vec![marked]));

        assert_eq!(
            plain_snapshot[0].structure_key,
            marked_snapshot[0].structure_key
        );
        assert!(plain_snapshot[0].leaves[0].format_key.is_empty());
        assert_eq!(marked_snapshot[0].leaves.len(), 1);
        assert!(marked_snapshot[0].leaves[0].format_key.contains("bold"));
        assert!(marked_snapshot[0].leaves[0].format_key.contains("link"));
    }

    #[test]
    fn block_type_and_attributes_live_in_structure() {
        let paragraph = Node::element("paragraph", vec![Node::text("Title", vec![])]);
        let heading = Node::element("heading", vec![Node::text("Title", vec![])])
            .with_attr("level", json!(2));
        let (_, _, paragraph_snapshot) = snapshots(Node::element("doc", vec![paragraph]));
        let (_, _, heading_snapshot) = snapshots(Node::element("doc", vec![heading]));

        assert_ne!(paragraph_snapshot[0].kind, heading_snapshot[0].kind);
        assert_ne!(
            paragraph_snapshot[0].structure_key,
            heading_snapshot[0].structure_key
        );
        assert!(heading_snapshot[0].structure_key.contains("level"));
        assert!(!heading_snapshot[0].structure_key.contains("Title"));
    }

    #[test]
    fn repeated_conversion_is_byte_for_byte_deterministic() {
        let tree = normalize(&Node::element(
            "doc",
            vec![
                Node::element(
                    "heading",
                    vec![Node::text("Stable", vec![Mark::new("italic")])],
                )
                .with_attr("level", json!(3)),
                Node::element("paragraph", vec![Node::text("Output", vec![])]),
            ],
        ));
        let document = Document::new();
        document.set_document(&tree);

        let first = block_snapshots(&document, &tree).unwrap();
        let second = block_snapshots(&document, &tree).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
    }

    #[test]
    fn prosemirror_ranges_map_to_global_grapheme_ranges() {
        let before = Node::element(
            "doc",
            vec![
                Node::element("paragraph", vec![Node::text("A", vec![])]),
                Node::element("paragraph", vec![Node::text("yesyes", vec![])]),
            ],
        );
        let after = Node::element(
            "doc",
            vec![
                Node::element("paragraph", vec![Node::text("A", vec![])]),
                Node::element("paragraph", vec![Node::text("yes", vec![])]),
            ],
        );

        // The second paragraph begins at document position 3 and its text at
        // position 4. Deleting the second `yes` therefore addresses 7..10.
        let anchors = semantic_range_anchors(
            &before,
            &after,
            &[ProseMirrorRangeHint {
                before_from: 7,
                before_to: 10,
                after_from: 7,
                after_to: 7,
            }],
        )
        .unwrap();
        assert_eq!(
            anchors,
            vec![SemanticRangeAnchor {
                before: 4..7,
                after: 4..4,
            }]
        );
    }

    #[test]
    fn structural_boundaries_map_to_empty_ranges() {
        let tree = Node::element(
            "doc",
            vec![
                Node::element("paragraph", vec![Node::text("A", vec![])]),
                Node::element("paragraph", vec![Node::text("B", vec![])]),
            ],
        );
        let anchors = semantic_range_anchors(
            &tree,
            &tree,
            &[ProseMirrorRangeHint {
                before_from: 3,
                before_to: 3,
                after_from: 3,
                after_to: 3,
            }],
        )
        .unwrap();
        assert_eq!(
            anchors,
            vec![SemanticRangeAnchor {
                before: 1..1,
                after: 1..1,
            }]
        );
    }

    #[test]
    fn horizontal_rule_uses_prosemirror_leaf_size_before_later_text() {
        let before = Node::element(
            "doc",
            vec![
                Node::element("horizontalRule", vec![]),
                Node::element("paragraph", vec![Node::text("A", vec![])]),
            ],
        );
        let after = Node::element(
            "doc",
            vec![
                Node::element("horizontalRule", vec![]),
                Node::element("paragraph", vec![Node::text("AB", vec![])]),
            ],
        );

        // A ProseMirror leaf contributes one position. The following
        // paragraph starts at 1, its text starts at 2, and appending after A
        // therefore changes 3..3 into 3..4.
        let anchors = semantic_range_anchors(
            &before,
            &after,
            &[ProseMirrorRangeHint {
                before_from: 3,
                before_to: 3,
                after_from: 3,
                after_to: 4,
            }],
        )
        .unwrap();
        assert_eq!(
            anchors,
            vec![SemanticRangeAnchor {
                before: 1..1,
                after: 1..2,
            }]
        );
    }

    #[test]
    fn expanded_composition_range_maps_the_complete_combining_grapheme() {
        let before = Node::element(
            "doc",
            vec![Node::element("paragraph", vec![Node::text("e", vec![])])],
        );
        let after = Node::element(
            "doc",
            vec![Node::element(
                "paragraph",
                vec![Node::text("e\u{301}", vec![])],
            )],
        );

        let anchors = semantic_range_anchors(
            &before,
            &after,
            &[ProseMirrorRangeHint {
                before_from: 1,
                before_to: 2,
                after_from: 1,
                after_to: 3,
            }],
        )
        .unwrap();
        assert_eq!(
            anchors,
            vec![SemanticRangeAnchor {
                before: 0..1,
                after: 0..1,
            }]
        );
    }

    #[test]
    fn positions_inside_a_grapheme_are_rejected() {
        let tree = Node::element(
            "doc",
            vec![Node::element("paragraph", vec![Node::text("🙂", vec![])])],
        );
        let error = semantic_range_anchors(
            &tree,
            &tree,
            &[ProseMirrorRangeHint {
                before_from: 2,
                before_to: 2,
                after_from: 1,
                after_to: 1,
            }],
        )
        .unwrap_err();
        assert_eq!(
            error,
            SnapshotError::NonGraphemePosition {
                side: "before",
                position: 2,
            }
        );
    }

    #[test]
    fn ranges_outside_the_document_are_rejected() {
        let tree = Node::element(
            "doc",
            vec![Node::element("paragraph", vec![Node::text("A", vec![])])],
        );
        let error = semantic_range_anchors(
            &tree,
            &tree,
            &[ProseMirrorRangeHint {
                before_from: 4,
                before_to: 4,
                after_from: 1,
                after_to: 1,
            }],
        )
        .unwrap_err();
        assert_eq!(
            error,
            SnapshotError::InvalidPositionRange {
                side: "before",
                from: 4,
                to: 4,
                size: 3,
            }
        );
    }
}
