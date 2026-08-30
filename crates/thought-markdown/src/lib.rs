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

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use thought_schema::Node;

mod parse;
mod serialize;

/// Projection metadata that preserves the Title/H1 distinction without
/// replacing readable Markdown headings with an opaque HTML block.
pub(crate) const TITLE_MARKER: &str = "<!--thought:title-->";

pub use thought_schema::normalize;

pub use parse::from_markdown;
pub use serialize::{to_markdown, to_markdown_with_spans};

/// Stable revision for the normalized wording and formatting currently visible.
pub fn current_wording_revision(doc: &Node) -> String {
    let normalized = normalize(doc);
    STANDARD.encode(Sha256::digest(to_markdown(&normalized).as_bytes()))
}

/// `parse(serialize(x))`, the operation the property test exercises.
pub fn round_trip(doc: &Node) -> Node {
    normalize(&from_markdown(&to_markdown(doc)))
}

#[cfg(test)]
mod revision_tests {
    use super::{current_wording_revision, from_markdown, normalize};

    #[test]
    fn wording_revision_is_stable_across_normalization() {
        let document = from_markdown("# Draft\n\nOne **careful** sentence.");
        assert_eq!(
            current_wording_revision(&document),
            current_wording_revision(&normalize(&document))
        );
    }

    #[test]
    fn wording_revision_changes_with_visible_text_or_formatting() {
        let plain = from_markdown("One sentence.");
        let bold = from_markdown("One **sentence**.");
        let changed = from_markdown("Two sentences.");
        assert_ne!(
            current_wording_revision(&plain),
            current_wording_revision(&bold)
        );
        assert_ne!(
            current_wording_revision(&plain),
            current_wording_revision(&changed)
        );
    }
}
