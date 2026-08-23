//! The document model and the schema that constrains it.
//!
//! Deliberately a faithful mirror of ProseMirror's JSON shape rather than a
//! prettier Rust-native design: this tree crosses to the editor unchanged, and
//! every divergence here becomes a translation layer later.

mod validate;
pub use validate::Violation;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Attrs = BTreeMap<String, serde_json::Value>;

/// A ProseMirror node. Text nodes carry `text` and `marks`; everything else
/// carries `content`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: Attrs,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<Node>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<Mark>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: Attrs,
}

impl Node {
    pub fn element(kind: &str, content: Vec<Node>) -> Self {
        Node {
            kind: kind.into(),
            attrs: Attrs::new(),
            content,
            text: None,
            marks: vec![],
        }
    }

    pub fn text(text: impl Into<String>, marks: Vec<Mark>) -> Self {
        Node {
            kind: "text".into(),
            attrs: Attrs::new(),
            content: vec![],
            text: Some(text.into()),
            marks,
        }
    }

    pub fn with_attr(mut self, key: &str, value: serde_json::Value) -> Self {
        self.attrs.insert(key.into(), value);
        self
    }

    pub fn attr_i64(&self, key: &str) -> Option<i64> {
        self.attrs.get(key)?.as_i64()
    }

    pub fn attr_str(&self, key: &str) -> Option<&str> {
        self.attrs.get(key)?.as_str()
    }

    pub fn is_text(&self) -> bool {
        self.kind == "text"
    }
}

impl Mark {
    pub fn new(kind: &str) -> Self {
        Mark {
            kind: kind.into(),
            attrs: Attrs::new(),
        }
    }

    pub fn with_attr(mut self, key: &str, value: serde_json::Value) -> Self {
        self.attrs.insert(key.into(), value);
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeSpec {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub code: bool,
    #[serde(default)]
    pub md: Option<String>,
    #[serde(default)]
    pub attrs: BTreeMap<String, AttrSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarkSpec {
    #[serde(default)]
    pub md: Option<String>,
    #[serde(default)]
    pub attrs: BTreeMap<String, AttrSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttrSpec {
    /// `None` means the key was absent, so the attribute is required.
    /// `Some(Value::Null)` means an explicit `"default": null` — optional, with
    /// a null default. Plain `Option` cannot tell these apart, because serde
    /// deserializes JSON null into `None`; the custom deserializer restores the
    /// distinction, and without it every nullable attr reads as required.
    #[serde(default, deserialize_with = "present_even_if_null")]
    pub default: Option<serde_json::Value>,
}

fn present_even_if_null<'de, D>(d: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(d).map(Some)
}

#[derive(Debug, Clone, Deserialize)]
pub struct Schema {
    pub nodes: BTreeMap<String, NodeSpec>,
    pub marks: BTreeMap<String, MarkSpec>,
}

const SCHEMA_JSON: &str = include_str!("../schema.json");

impl Schema {
    /// The v0 schema, compiled in. Panics on a malformed schema.json, which is
    /// correct: a broken schema is not a runtime condition to handle.
    pub fn v0() -> &'static Schema {
        use std::sync::OnceLock;
        static SCHEMA: OnceLock<Schema> = OnceLock::new();
        SCHEMA.get_or_init(|| serde_json::from_str(SCHEMA_JSON).expect("schema.json is malformed"))
    }

    pub fn node(&self, kind: &str) -> Option<&NodeSpec> {
        self.nodes.get(kind)
    }

    pub fn mark(&self, kind: &str) -> Option<&MarkSpec> {
        self.marks.get(kind)
    }
}

/// Put a tree in normal form: adjacent text nodes with equal marks merged,
/// empty text dropped, mark order canonicalized.
///
/// Not a markdown concern despite originating there. Any representation that
/// stores marks as ranges beside the characters — markdown *and* the CRDT —
/// collapses two adjacent identical-mark runs into one, so trees must be
/// normalized before equality between them means anything.
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
    const ORDER: &[&str] = &["link", "bold", "italic", "strike", "code"];
    let rank = |m: &Mark| {
        ORDER
            .iter()
            .position(|k| *k == m.kind)
            .unwrap_or(usize::MAX)
    };
    let mut out = marks.to_vec();
    out.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.kind.cmp(&b.kind)));
    out.dedup_by(|a, b| a.kind == b.kind);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_loads_and_has_the_v0_set() {
        let s = Schema::v0();
        for kind in [
            "doc",
            "paragraph",
            "heading",
            "codeBlock",
            "listItem",
            "text",
        ] {
            assert!(s.node(kind).is_some(), "missing node {kind}");
        }
        for kind in ["bold", "italic", "code", "link"] {
            assert!(s.mark(kind).is_some(), "missing mark {kind}");
        }
        assert!(s.node("codeBlock").unwrap().code);
    }

    #[test]
    fn node_json_matches_prosemirror_shape() {
        let n = Node::element("paragraph", vec![Node::text("hi", vec![Mark::new("bold")])]);
        let json = serde_json::to_value(&n).unwrap();
        assert_eq!(json["type"], "paragraph");
        assert_eq!(json["content"][0]["text"], "hi");
        assert_eq!(json["content"][0]["marks"][0]["type"], "bold");
        // absent rather than null: ProseMirror omits empty fields
        assert!(json.get("attrs").is_none());
    }
}
