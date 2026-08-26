//! Adapter from the live CRDT/tree representation to semantic-lineage input.
//!
//! The provenance engine deliberately knows nothing about Yjs or the
//! ProseMirror schema. This module is the narrow boundary: it pairs normalized
//! top-level nodes with their intrinsic CRDT block ids, records text-leaf
//! paths, and reduces rich structure and marks to deterministic canonical keys.

use serde::Serialize;
use std::fmt;
use thought_core::Document;
use thought_provenance::{BlockSnapshot, TextLeafSnapshot};
use thought_schema::{Attrs, Mark, Node};

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
        }
    }
}

impl std::error::Error for SnapshotError {}

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
}
