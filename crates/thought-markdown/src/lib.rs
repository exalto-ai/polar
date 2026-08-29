//! The markdown projection (M1.5).
//!
//! Lives in Rust rather than JS because AD-2 requires the daemon to serve
//! markdown to agents with no window open. Both directions matter: agents read
//! the serialized form and write edits that must parse back into the same tree.
//!
//! The contract is `parse(serialize(doc)) == doc` for any *normalized* document.
//! Normalization is not a caveat hiding a bug — markdown genuinely cannot
//! distinguish one text node from two adjacent ones carrying identical marks,
//! so the tree must be in normal form before the equality is meaningful.

use thought_schema::Node;

mod parse;
mod serialize;

/// Projection metadata that preserves the Title/H1 distinction without
/// replacing readable Markdown headings with an opaque HTML block.
pub(crate) const TITLE_MARKER: &str = "<!--thought:title-->";

pub use thought_schema::normalize;

pub use parse::from_markdown;
pub use serialize::{to_markdown, to_markdown_with_spans};

/// `parse(serialize(x))`, the operation the property test exercises.
pub fn round_trip(doc: &Node) -> Node {
    normalize(&from_markdown(&to_markdown(doc)))
}
