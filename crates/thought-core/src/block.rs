//! Block identity and the block-scoped operations agents use (AD-5, M1.3).
//!
//! Identity is intrinsic: every `XmlElement` carries a yrs `BranchID`, which is
//! unique without coordination, stable across edits to the block's contents,
//! and — the property that actually matters — identical on every replica.
//! Verified in `prototypes/yrs-check` before any of this was written.

use crate::Document;
use crate::tree;
use thought_schema::Node;
use yrs::branch::{Branch, BranchID};
use yrs::types::text::YChange;
use yrs::types::xml::{XmlElementPrelim, XmlFragment, XmlOut};
use yrs::{Text, TransactionMut, Xml, XmlElementRef, XmlFragmentRef};

/// A top-level block and its stable address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRef {
    pub block_id: String,
    pub kind: String,
    pub index: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BlockError {
    /// The block is gone — split, deleted, or replaced by another actor. Not an
    /// error condition so much as news: the caller read a document that has
    /// since moved, and needs current anchors (M1.6).
    NoSuchBlock(String),
}

impl std::fmt::Display for BlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockError::NoSuchBlock(id) => {
                write!(f, "block `{id}` no longer exists; re-read the document")
            }
        }
    }
}

impl std::error::Error for BlockError {}

/// Where to place newly inserted blocks.
#[derive(Debug, Clone)]
pub enum Position {
    Start,
    End,
    After(String),
}

pub(crate) fn block_id(element: &impl AsRef<Branch>) -> String {
    match element.as_ref().id() {
        BranchID::Nested(id) => format!("{}:{}", id.client, id.clock),
        BranchID::Root(name) => format!("root:{name}"),
    }
}

impl Document {
    fn find(
        &self,
        txn: &impl yrs::ReadTxn,
        fragment: &XmlFragmentRef,
        id: &str,
    ) -> Option<(u32, XmlElementRef)> {
        (0..fragment.len(txn)).find_map(|i| match fragment.get(txn, i) {
            Some(XmlOut::Element(element)) if block_id(&element) == id => Some((i, element)),
            _ => None,
        })
    }

    /// Every top-level block, in order.
    pub fn blocks(&self) -> Vec<BlockRef> {
        let fragment = self.fragment_ref();
        let txn = self.transact();
        (0..fragment.len(&txn))
            .filter_map(|i| match fragment.get(&txn, i) {
                Some(XmlOut::Element(element)) => Some(BlockRef {
                    block_id: block_id(&element),
                    kind: element.tag().to_string(),
                    index: i as usize,
                }),
                _ => None,
            })
            .collect()
    }

    /// Replace a block's contents.
    ///
    /// When the node type is unchanged the element is edited in place, so the
    /// `block_id` survives and anchors held by other actors stay valid. Changing
    /// the type — paragraph to heading — cannot preserve identity, because an
    /// `XmlElement`'s tag is immutable; the returned `BlockRef` then carries the
    /// new id rather than leaving the caller to discover it.
    pub fn replace_block(&self, id: &str, node: &Node) -> Result<BlockRef, BlockError> {
        let fragment = self.fragment_ref();
        let mut txn = self.transact_mut();
        let (index, element) = self
            .find(&txn, &fragment, id)
            .ok_or_else(|| BlockError::NoSuchBlock(id.to_string()))?;

        if element.tag().as_ref() == node.kind {
            let len = element.len(&txn);
            if len > 0 {
                element.remove_range(&mut txn, 0, len);
            }
            for (key, value) in &node.attrs {
                element.insert_attribute(
                    &mut txn,
                    key.as_str(),
                    crate::convert::json_to_any(value),
                );
            }
            tree::write_children(&mut txn, &element, &node.content);
            return Ok(BlockRef {
                block_id: id.to_string(),
                kind: node.kind.clone(),
                index: index as usize,
            });
        }

        fragment.remove_range(&mut txn, index, 1);
        let created = insert_one(&mut txn, &fragment, index, node);
        Ok(BlockRef {
            block_id: block_id(&created),
            kind: node.kind.clone(),
            index: index as usize,
        })
    }

    pub fn insert_blocks(
        &self,
        at: &Position,
        nodes: &[Node],
    ) -> Result<Vec<BlockRef>, BlockError> {
        let fragment = self.fragment_ref();
        let mut txn = self.transact_mut();

        let index = match at {
            Position::Start => 0,
            Position::End => fragment.len(&txn),
            Position::After(id) => {
                let (i, _) = self
                    .find(&txn, &fragment, id)
                    .ok_or_else(|| BlockError::NoSuchBlock(id.clone()))?;
                i + 1
            }
        };

        let out = nodes
            .iter()
            .enumerate()
            .map(|(offset, node)| {
                let at = index + offset as u32;
                let element = insert_one(&mut txn, &fragment, at, node);
                BlockRef {
                    block_id: block_id(&element),
                    kind: node.kind.clone(),
                    index: at as usize,
                }
            })
            .collect();
        Ok(out)
    }

    pub fn delete_block(&self, id: &str) -> Result<(), BlockError> {
        let fragment = self.fragment_ref();
        let mut txn = self.transact_mut();
        let (index, _) = self
            .find(&txn, &fragment, id)
            .ok_or_else(|| BlockError::NoSuchBlock(id.to_string()))?;
        fragment.remove_range(&mut txn, index, 1);
        Ok(())
    }

    /// Append text to a block's inline run, leaving the rest untouched.
    ///
    /// Distinct from `replace_block` on purpose: this is the fine-grained edit
    /// whose concurrent behaviour actually exercises the CRDT, where two actors
    /// appending to the same paragraph both keep their text.
    pub fn append_text(&self, id: &str, text: &str) -> Result<(), BlockError> {
        let fragment = self.fragment_ref();
        let mut txn = self.transact_mut();
        let (_, element) = self
            .find(&txn, &fragment, id)
            .ok_or_else(|| BlockError::NoSuchBlock(id.to_string()))?;

        let existing = (0..element.len(&txn)).find_map(|i| match element.get(&txn, i) {
            Some(XmlOut::Text(t)) => Some(t),
            _ => None,
        });

        match existing {
            Some(run) => {
                let at = run.len(&txn);
                run.insert(&mut txn, at, text);
            }
            None => {
                let run = element.push_back(&mut txn, yrs::types::xml::XmlTextPrelim::new(""));
                run.insert(&mut txn, 0, text);
            }
        }
        Ok(())
    }

    /// One block as a ProseMirror node, or `None` if it no longer exists.
    pub fn block(&self, id: &str) -> Option<Node> {
        let fragment = self.fragment_ref();
        let txn = self.transact();
        let (_, element) = self.find(&txn, &fragment, id)?;
        let mut node = Node::element(element.tag().as_ref(), tree::read_children(&txn, &element));
        for (key, value) in element.attributes(&txn) {
            if let yrs::Out::Any(any) = value {
                node.attrs
                    .insert(key.to_string(), crate::convert::any_to_json(&any));
            }
        }
        Some(node)
    }

    /// The plain text of a block, ignoring marks.
    pub fn block_text(&self, id: &str) -> Result<String, BlockError> {
        let fragment = self.fragment_ref();
        let txn = self.transact();
        let (_, element) = self
            .find(&txn, &fragment, id)
            .ok_or_else(|| BlockError::NoSuchBlock(id.to_string()))?;

        let mut out = String::new();
        for i in 0..element.len(&txn) {
            if let Some(XmlOut::Text(run)) = element.get(&txn, i) {
                for chunk in run.diff(&txn, YChange::identity) {
                    if let yrs::Out::Any(yrs::Any::String(s)) = chunk.insert {
                        out.push_str(&s);
                    }
                }
            }
        }
        Ok(out)
    }
}

fn insert_one(
    txn: &mut TransactionMut,
    fragment: &XmlFragmentRef,
    index: u32,
    node: &Node,
) -> XmlElementRef {
    let element = fragment.insert(txn, index, XmlElementPrelim::empty(node.kind.as_str()));
    for (key, value) in &node.attrs {
        element.insert_attribute(txn, key.as_str(), crate::convert::json_to_any(value));
    }
    tree::write_children(txn, &element, &node.content);
    element
}
