use thought_core::Document;
use thought_provenance::{BlockSnapshot, TextLeafSnapshot};
use thought_schema::Node;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    NotDocument,
    BlockCount,
    BlockKind,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotDocument => "lineage snapshot is not a document",
            Self::BlockCount => "lineage snapshot block count does not match the CRDT",
            Self::BlockKind => "lineage snapshot block kind does not match the CRDT",
        })
    }
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
