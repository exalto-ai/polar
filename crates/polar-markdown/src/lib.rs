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

use polar_schema::{Mark, Node};

mod parse;
mod serialize;

pub use parse::from_markdown;
pub use serialize::to_markdown;

/// Collapse a tree into the form markdown can represent: adjacent text nodes
/// with equal marks merged, empty text dropped, mark order canonicalized.
pub fn normalize(node: &Node) -> Node {
    let mut out = node.clone();

    if out.is_text() {
        out.marks = canonical_marks(&out.marks);
        return out;
    }

    let mut content: Vec<Node> = Vec::with_capacity(out.content.len());
    for child in &out.content {
        let child = normalize(child);

        if child.is_text() {
            if child.text.as_deref().is_none_or(str::is_empty) {
                continue;
            }
            if let Some(prev) = content.last_mut()
                && prev.is_text()
                && prev.marks == child.marks
            {
                let merged = format!(
                    "{}{}",
                    prev.text.as_deref().unwrap_or(""),
                    child.text.as_deref().unwrap_or("")
                );
                prev.text = Some(merged);
                continue;
            }
        }
        content.push(child);
    }

    out.content = content;
    out
}

/// Marks are a set, not a sequence, but the tree stores them as a Vec — so a
/// stable order is required before two trees can be compared.
fn canonical_marks(marks: &[Mark]) -> Vec<Mark> {
    const ORDER: &[&str] = &["link", "strong", "em", "strike", "code"];
    let rank = |m: &Mark| ORDER.iter().position(|k| *k == m.kind).unwrap_or(usize::MAX);
    let mut out = marks.to_vec();
    out.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.kind.cmp(&b.kind)));
    out.dedup_by(|a, b| a.kind == b.kind);
    out
}

/// `parse(serialize(x))`, the operation the property test exercises.
pub fn round_trip(doc: &Node) -> Node {
    normalize(&from_markdown(&to_markdown(doc)))
}
