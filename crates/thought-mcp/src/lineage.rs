use thought_core::Document;
use thought_provenance::{BlockSnapshot, SemanticRange, TextLeafSnapshot};
use thought_schema::Node;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProseMirrorRange {
    pub before_from: u32,
    pub before_to: u32,
    pub after_from: u32,
    pub after_to: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    NotDocument,
    BlockCount,
    BlockKind,
    InvalidPosition,
    PositionOverflow,
    NonGraphemePosition,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotDocument => "lineage snapshot is not a document",
            Self::BlockCount => "lineage snapshot block count does not match the CRDT",
            Self::BlockKind => "lineage snapshot block kind does not match the CRDT",
            Self::InvalidPosition => "editor range is outside the document",
            Self::PositionOverflow => "document is too large for editor positions",
            Self::NonGraphemePosition => "editor range splits a Unicode grapheme",
        })
    }
}

pub fn semantic_ranges(
    before: &Node,
    after: &Node,
    ranges: &[ProseMirrorRange],
) -> Result<Vec<SemanticRange>, SnapshotError> {
    let before = PositionedDocument::new(before)?;
    let after = PositionedDocument::new(after)?;
    ranges
        .iter()
        .map(|range| {
            Ok(SemanticRange {
                before: before.range(range.before_from, range.before_to)?,
                after: after.range(range.after_from, range.after_to)?,
            })
        })
        .collect()
}

impl std::error::Error for SnapshotError {}

pub fn block_snapshots(
    document: &Document,
    tree: &Node,
) -> Result<Vec<BlockSnapshot>, SnapshotError> {
    if tree.kind != "doc" {
        return Err(SnapshotError::NotDocument);
    }
    let blocks = document.blocks();
    if blocks.len() != tree.content.len() {
        return Err(SnapshotError::BlockCount);
    }
    blocks
        .into_iter()
        .zip(&tree.content)
        .map(|(block, node)| {
            if block.kind != node.kind {
                return Err(SnapshotError::BlockKind);
            }
            let mut leaves = Vec::new();
            collect_leaves(node, &mut Vec::new(), &mut leaves);
            Ok(BlockSnapshot::new(block.block_id, leaves))
        })
        .collect()
}

fn collect_leaves(node: &Node, path: &mut Vec<u32>, out: &mut Vec<TextLeafSnapshot>) {
    for (index, child) in node.content.iter().enumerate() {
        path.push(index as u32);
        if child.is_text() {
            if let Some(text) = child.text.as_deref()
                && !text.is_empty()
            {
                out.push(TextLeafSnapshot::new(path.clone(), text));
            }
        } else {
            collect_leaves(child, path, out);
        }
        path.pop();
    }
}

#[derive(Debug, Clone, Copy)]
struct PositionedGrapheme {
    from: u32,
    to: u32,
}

struct PositionedDocument {
    size: u32,
    graphemes: Vec<PositionedGrapheme>,
}

impl PositionedDocument {
    fn new(document: &Node) -> Result<Self, SnapshotError> {
        if document.kind != "doc" {
            return Err(SnapshotError::NotDocument);
        }
        let mut graphemes = Vec::new();
        let mut position = 0;
        for node in &document.content {
            collect_positions(node, position, &mut graphemes)?;
            position = position
                .checked_add(node_size(node)?)
                .ok_or(SnapshotError::PositionOverflow)?;
        }
        Ok(Self {
            size: position,
            graphemes,
        })
    }

    fn range(&self, from: u32, to: u32) -> Result<std::ops::Range<usize>, SnapshotError> {
        if from > to || to > self.size {
            return Err(SnapshotError::InvalidPosition);
        }
        if [from, to].into_iter().any(|position| {
            self.graphemes
                .iter()
                .any(|item| item.from < position && position < item.to)
        }) {
            return Err(SnapshotError::NonGraphemePosition);
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

fn node_size(node: &Node) -> Result<u32, SnapshotError> {
    if node.is_text() {
        return u32::try_from(
            node.text
                .as_deref()
                .unwrap_or_default()
                .encode_utf16()
                .count(),
        )
        .map_err(|_| SnapshotError::PositionOverflow);
    }
    if node.kind == "horizontalRule" {
        return Ok(1);
    }
    node.content.iter().try_fold(2_u32, |size, child| {
        size.checked_add(node_size(child)?)
            .ok_or(SnapshotError::PositionOverflow)
    })
}

fn collect_positions(
    node: &Node,
    start: u32,
    out: &mut Vec<PositionedGrapheme>,
) -> Result<(), SnapshotError> {
    if node.is_text() {
        let mut offset = 0_u32;
        for grapheme in node.text.as_deref().unwrap_or_default().graphemes(true) {
            let from = start
                .checked_add(offset)
                .ok_or(SnapshotError::PositionOverflow)?;
            offset = offset
                .checked_add(
                    u32::try_from(grapheme.encode_utf16().count())
                        .map_err(|_| SnapshotError::PositionOverflow)?,
                )
                .ok_or(SnapshotError::PositionOverflow)?;
            out.push(PositionedGrapheme {
                from,
                to: start
                    .checked_add(offset)
                    .ok_or(SnapshotError::PositionOverflow)?,
            });
        }
        return Ok(());
    }
    if node.kind == "horizontalRule" {
        return Ok(());
    }
    let mut child_start = start
        .checked_add(1)
        .ok_or(SnapshotError::PositionOverflow)?;
    for child in &node.content {
        collect_positions(child, child_start, out)?;
        child_start = child_start
            .checked_add(node_size(child)?)
            .ok_or(SnapshotError::PositionOverflow)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(text: &str) -> Node {
        Node::element(
            "doc",
            vec![Node::element(
                "paragraph",
                (!text.is_empty())
                    .then(|| Node::text(text, vec![]))
                    .into_iter()
                    .collect(),
            )],
        )
    }

    #[test]
    fn empty_paragraph_positions_map_to_an_insertion() {
        let ranges = semantic_ranges(
            &document(""),
            &document("text"),
            &[ProseMirrorRange {
                before_from: 1,
                before_to: 1,
                after_from: 1,
                after_to: 5,
            }],
        )
        .unwrap();
        assert_eq!(ranges[0].before, 0..0);
        assert_eq!(ranges[0].after, 0..4);
    }

    #[test]
    fn a_position_inside_an_emoji_is_not_exact() {
        assert_eq!(
            semantic_ranges(
                &document("a🙂b"),
                &document("a🙂b"),
                &[ProseMirrorRange {
                    before_from: 3,
                    before_to: 3,
                    after_from: 3,
                    after_to: 3,
                }],
            ),
            Err(SnapshotError::NonGraphemePosition),
        );
    }
}
