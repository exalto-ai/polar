//! The CRDT document: a yrs `Doc` holding a ProseMirror tree, addressable by
//! stable block identity.

mod block;
mod convert;
mod tree;

pub use block::{BlockError, BlockRef, Position};

use polar_schema::Node;
use yrs::types::xml::XmlFragment;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update, XmlFragmentRef};

/// Applying a peer's update can fail two ways, and they mean different things:
/// a malformed frame is a transport or version problem, while an update error
/// is a document-integrity problem. Collapsing them would hide which.
#[derive(Debug)]
pub enum ApplyError {
    Decode(yrs::encoding::read::Error),
    Apply(yrs::error::UpdateError),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Decode(e) => write!(f, "malformed update frame: {e}"),
            ApplyError::Apply(e) => write!(f, "update could not be applied: {e}"),
        }
    }
}

impl std::error::Error for ApplyError {}

pub use tree::sort_marks;

/// The root fragment name. Matches the editor binding, which must agree.
pub const CONTENT: &str = "content";

pub struct Document {
    doc: Doc,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        Document { doc: Doc::new() }
    }

    fn fragment(&self) -> XmlFragmentRef {
        self.doc.get_or_insert_xml_fragment(CONTENT)
    }

    pub(crate) fn fragment_ref(&self) -> XmlFragmentRef {
        self.fragment()
    }

    pub(crate) fn transact(&self) -> yrs::Transaction<'_> {
        self.doc.transact()
    }

    pub(crate) fn transact_mut(&self) -> yrs::TransactionMut<'_> {
        self.doc.transact_mut()
    }

    /// Replace the whole document. Only for constructing test fixtures — agents
    /// never do this (AD-5), because a whole-document write destroys concurrent
    /// edits and makes attribution meaningless.
    pub fn set_document(&self, node: &Node) {
        let fragment = self.fragment();
        let mut txn = self.doc.transact_mut();
        let len = fragment.len(&txn);
        if len > 0 {
            fragment.remove_range(&mut txn, 0, len);
        }
        tree::write_children(&mut txn, &fragment, &node.content);
    }

    /// Read the document as a ProseMirror tree.
    pub fn read(&self) -> Node {
        let fragment = self.fragment();
        let txn = self.doc.transact();
        Node::element("doc", tree::read_children(&txn, &fragment))
    }

    pub fn state_vector(&self) -> Vec<u8> {
        self.doc.transact().state_vector().encode_v1()
    }

    /// Everything this replica has that the holder of `state_vector` does not.
    pub fn diff_since(&self, state_vector: &[u8]) -> Vec<u8> {
        let sv = StateVector::decode_v1(state_vector).unwrap_or_default();
        self.doc.transact().encode_diff_v1(&sv)
    }

    pub fn encode_state(&self) -> Vec<u8> {
        self.doc.transact().encode_diff_v1(&StateVector::default())
    }

    pub fn apply_update(&self, update: &[u8]) -> Result<(), ApplyError> {
        let update = Update::decode_v1(update).map_err(ApplyError::Decode)?;
        self.doc
            .transact_mut()
            .apply_update(update)
            .map_err(ApplyError::Apply)
    }
}
