//! ProseMirror tree <-> yrs `XmlFragment`.
//!
//! Block nodes become `XmlElement`s; a run of inline children collapses into a
//! single `XmlText` whose marks are formatting attributes. That shape is what
//! makes concurrent editing work at all: marks live *beside* the characters
//! rather than inside them, so one actor bolding a phrase and another rewriting
//! it merge instead of colliding (AD-3).

use crate::convert::{any_map_to_attrs, any_to_json, json_to_any};
use std::sync::Arc;
use thought_schema::{Mark, Node};
use yrs::types::Attrs as YAttrs;
use yrs::types::text::YChange;
use yrs::types::xml::{XmlElementPrelim, XmlFragment, XmlOut, XmlTextPrelim};
use yrs::{Any, Out, ReadTxn, Text, TransactionMut, Xml, XmlTextRef};

/// A mark becomes one formatting attribute keyed by mark type. Marks without
/// attributes store `true`; marks with attributes store a map.
fn marks_to_attrs(marks: &[Mark]) -> YAttrs {
    marks
        .iter()
        .map(|mark| {
            let value = if mark.attrs.is_empty() {
                Any::Bool(true)
            } else {
                Any::Map(Arc::new(
                    mark.attrs
                        .iter()
                        .map(|(k, v)| (k.clone(), json_to_any(v)))
                        .collect(),
                ))
            };
            (Arc::from(mark.kind.as_str()), value)
        })
        .collect()
}

fn attrs_to_marks(attrs: Option<&YAttrs>) -> Vec<Mark> {
    let Some(attrs) = attrs else {
        return vec![];
    };
    let mut marks: Vec<Mark> = attrs
        .iter()
        // yrs clears formatting by writing null, so a null attribute means the
        // mark is absent rather than present-with-no-value.
        .filter(|(_, value)| !matches!(value, Any::Null | Any::Undefined))
        .map(|(kind, value)| {
            let mut mark = Mark::new(kind);
            if let Any::Map(map) = value {
                mark.attrs = any_map_to_attrs(map);
            }
            mark
        })
        .collect();
    marks.sort_by(|a, b| a.kind.cmp(&b.kind));
    marks
}

/// Write `children` into `parent`, which is either the root fragment or an
/// element.
pub fn write_children<F>(txn: &mut TransactionMut, parent: &F, children: &[Node])
where
    F: XmlFragment,
{
    if !children.is_empty() && children.iter().all(Node::is_text) {
        let text = parent.push_back(txn, XmlTextPrelim::new(""));
        for child in children {
            let s = child.text.as_deref().unwrap_or("");
            if s.is_empty() {
                continue;
            }
            // Insert at the current end rather than a tracked offset: yrs
            // indices follow the document's offset encoding, and asking for the
            // length avoids having to agree with it.
            let at = text.len(txn);
            text.insert_with_attributes(txn, at, s, marks_to_attrs(&child.marks));
        }
        return;
    }

    for child in children {
        let element = parent.push_back(txn, XmlElementPrelim::empty(child.kind.as_str()));
        for (key, value) in &child.attrs {
            element.insert_attribute(txn, key.as_str(), json_to_any(value));
        }
        write_children(txn, &element, &child.content);
    }
}

fn read_text(txn: &impl ReadTxn, text: &XmlTextRef) -> Vec<Node> {
    text.diff(txn, YChange::identity)
        .into_iter()
        .filter_map(|chunk| match chunk.insert {
            Out::Any(Any::String(s)) => Some(Node::text(
                s.to_string(),
                attrs_to_marks(chunk.attributes.as_deref()),
            )),
            _ => None,
        })
        .collect()
}

/// Read the children of `parent` back into ProseMirror nodes.
pub fn read_children<F>(txn: &impl ReadTxn, parent: &F) -> Vec<Node>
where
    F: XmlFragment,
{
    let mut out = Vec::new();
    for i in 0..parent.len(txn) {
        match parent.get(txn, i) {
            Some(XmlOut::Text(text)) => out.extend(read_text(txn, &text)),
            Some(XmlOut::Element(element)) => {
                let kind = element.tag().to_string();
                let mut node = Node::element(&kind, read_children(txn, &element));
                for (key, value) in element.attributes(txn) {
                    let json = match value {
                        Out::Any(any) => any_to_json(&any),
                        other => serde_json::Value::String(other.to_string(txn)),
                    };
                    node.attrs.insert(key.to_string(), json);
                }
                out.push(node);
            }
            Some(XmlOut::Fragment(fragment)) => out.extend(read_children(txn, &fragment)),
            None => {}
        }
    }
    out
}

/// Marks are a set; the tree stores a Vec, so a stable order is needed before
/// two trees can be compared. Mirrors `thought_markdown::normalize`.
pub fn sort_marks(node: &mut Node) {
    node.marks.sort_by(|a, b| a.kind.cmp(&b.kind));
    for child in &mut node.content {
        sort_marks(child);
    }
}
