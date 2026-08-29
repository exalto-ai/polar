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

/// Font-size values are persisted in Yjs and rendered as inline CSS, so the
/// allowed representation is deliberately narrow and canonical.
pub const MIN_FONT_SIZE_PX: u16 = 8;
pub const MAX_FONT_SIZE_PX: u16 = 96;

pub fn normalize_font_size(value: &str) -> Option<String> {
    let pixels = value.strip_suffix("px")?;
    if pixels.is_empty() || !pixels.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    let pixels: u16 = pixels.parse().ok()?;
    (MIN_FONT_SIZE_PX..=MAX_FONT_SIZE_PX)
        .contains(&pixels)
        .then(|| format!("{pixels}px"))
}

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

    /// Read an integer attribute, however it was encoded.
    ///
    /// y-prosemirror writes numeric attributes as JavaScript numbers, which
    /// cross as floats — and `as_i64` returns `None` for a float, so a heading
    /// written in the editor silently read back as level 1. Some clients send
    /// them as strings too. The document model should not care which.
    pub fn attr_i64(&self, key: &str) -> Option<i64> {
        let value = self.attrs.get(key)?;
        value
            .as_i64()
            .or_else(|| {
                value
                    .as_f64()
                    .filter(|number| number.fract() == 0.0)
                    .map(|number| number as i64)
            })
            .or_else(|| value.as_str()?.parse().ok())
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

    // TipTap materializes nullable defaults in `node.attrs`; Rust parsers omit
    // defaults. Both spellings mean an ordinary heading, so absence is the
    // canonical form and Title remains the only persisted variant value.
    if out.kind == "heading"
        && out
            .attrs
            .get("variant")
            .is_some_and(serde_json::Value::is_null)
    {
        out.attrs.remove("variant");
    }

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
    // fontSize must wrap code in the Markdown projection. If it were inside a
    // code fence, its HTML span would become literal code and the mark would
    // disappear on parse.
    const ORDER: &[&str] = &["link", "bold", "italic", "strike", "fontSize", "code"];
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
        for kind in ["bold", "italic", "code", "link", "fontSize"] {
            assert!(s.mark(kind).is_some(), "missing mark {kind}");
        }
        assert!(s.node("codeBlock").unwrap().code);
    }

    /// The shape y-prosemirror actually sends. A float `level` read back as
    /// `None` turned every heading written in the editor into an h1.
    #[test]
    fn integer_attrs_survive_however_they_were_encoded() {
        let cases = [
            serde_json::json!(2),
            serde_json::json!(2.0),
            serde_json::json!("2"),
        ];
        for value in cases {
            let node = Node::element("heading", vec![]).with_attr("level", value.clone());
            assert_eq!(node.attr_i64("level"), Some(2), "failed for {value}");
        }

        let fractional =
            Node::element("heading", vec![]).with_attr("level", serde_json::json!(1.9));
        assert_eq!(fractional.attr_i64("level"), None);
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

    #[test]
    fn font_sizes_have_one_safe_canonical_representation() {
        assert_eq!(normalize_font_size("8px").as_deref(), Some("8px"));
        assert_eq!(normalize_font_size("18px").as_deref(), Some("18px"));
        assert_eq!(normalize_font_size("96px").as_deref(), Some("96px"));

        assert_eq!(normalize_font_size("7px"), None);
        assert_eq!(normalize_font_size("97px"), None);
        assert_eq!(normalize_font_size("18.5px"), None);
        assert_eq!(normalize_font_size("1rem"), None);
        assert_eq!(normalize_font_size("18px; color:red"), None);

        // Parsing may accept an equivalent numeric spelling, but persisted
        // values and serializers always use the canonical form.
        assert_eq!(normalize_font_size("018px").as_deref(), Some("18px"));
    }
}
