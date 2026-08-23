//! Validation against the content expressions in `schema.json`.
//!
//! This exists because of a specific recurring mistake: twice during M1 step 1
//! a property-test failure looked like a serializer bug and was in fact a
//! document the schema forbids — a `listItem` missing its leading paragraph,
//! and a ragged table. Both times the tempting fix was to make the serializer
//! accommodate a tree no editor could produce. Rejecting invalid trees at the
//! boundary removes the whole class.

use crate::{Node, Schema};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    /// Where in the tree, as a node-type path: `doc > listItem > 0`.
    pub path: String,
    pub message: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// One term of a content expression: `block*`, `paragraph`, or an alternation
/// like `(tableCell | tableHeader)+`.
struct Term {
    /// Any of these satisfies the term.
    names: Vec<String>,
    min: usize,
    max: usize,
}

impl Term {
    fn describe(&self) -> String {
        self.names.join(" | ")
    }
}

/// Split on whitespace, but keep a parenthesised alternation whole.
fn tokenize(expr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for ch in expr.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            c if c.is_whitespace() && depth == 0 => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn parse_expr(expr: &str) -> Vec<Term> {
    tokenize(expr)
        .into_iter()
        .map(|tok| {
            let (body, min, max) = match tok.chars().last() {
                Some('+') => (&tok[..tok.len() - 1], 1, usize::MAX),
                Some('*') => (&tok[..tok.len() - 1], 0, usize::MAX),
                Some('?') => (&tok[..tok.len() - 1], 0, 1),
                _ => (tok.as_str(), 1, 1),
            };
            let names = body
                .trim_start_matches('(')
                .trim_end_matches(')')
                .split('|')
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect();
            Term { names, min, max }
        })
        .collect()
}

impl Schema {
    /// Does `node` satisfy `name`, either as its type or via its group?
    ///
    /// A ProseMirror `group` is a whitespace-separated *set*, not a single
    /// name — `bulletList` belongs to `"block list"`. Comparing the whole
    /// string put every list outside `block+`.
    fn matches(&self, node: &Node, name: &str) -> bool {
        if node.kind == name {
            return true;
        }
        self.node(&node.kind)
            .and_then(|spec| spec.group.as_deref())
            .is_some_and(|groups| groups.split_whitespace().any(|g| g == name))
    }

    pub fn validate(&self, root: &Node) -> Result<(), Vec<Violation>> {
        let mut out = Vec::new();
        self.walk(root, &root.kind, &mut out);
        if out.is_empty() { Ok(()) } else { Err(out) }
    }

    fn walk(&self, node: &Node, path: &str, out: &mut Vec<Violation>) {
        let fail = |out: &mut Vec<Violation>, message: String| {
            out.push(Violation {
                path: path.to_string(),
                message,
            });
        };

        let Some(spec) = self.node(&node.kind) else {
            fail(out, format!("unknown node type `{}`", node.kind));
            return;
        };

        if node.is_text() {
            if node.text.is_none() {
                fail(out, "text node carries no text".into());
            }
            for mark in &node.marks {
                match self.mark(&mark.kind) {
                    None => fail(out, format!("unknown mark `{}`", mark.kind)),
                    Some(mspec) => {
                        for (key, aspec) in &mspec.attrs {
                            if aspec.default.is_none() && !mark.attrs.contains_key(key) {
                                fail(out, format!("mark `{}` requires attr `{key}`", mark.kind));
                            }
                        }
                    }
                }
            }
        } else if !node.marks.is_empty() {
            fail(
                out,
                format!("`{}` is not a text node but carries marks", node.kind),
            );
        }

        for (key, aspec) in &spec.attrs {
            if aspec.default.is_none() && !node.attrs.contains_key(key) {
                fail(out, format!("`{}` requires attr `{key}`", node.kind));
            }
        }

        match spec.content.as_deref() {
            None => {
                if !node.content.is_empty() {
                    fail(out, format!("`{}` takes no content", node.kind));
                }
            }
            Some(expr) => self.check_content(node, expr, path, out),
        }

        for (i, child) in node.content.iter().enumerate() {
            let child_path = format!("{path} > {}[{i}]", child.kind);
            self.walk(child, &child_path, out);
        }
    }

    /// Greedy left-to-right match, with alternation inside a term. Sufficient
    /// for v0, which never places a required term after an unbounded one; that
    /// would need real backtracking.
    fn check_content(&self, node: &Node, expr: &str, path: &str, out: &mut Vec<Violation>) {
        let terms = parse_expr(expr);
        let mut i = 0;

        for term in &terms {
            let mut count = 0;
            while i < node.content.len()
                && count < term.max
                && term.names.iter().any(|n| self.matches(&node.content[i], n))
            {
                i += 1;
                count += 1;
            }
            if count < term.min {
                out.push(Violation {
                    path: path.to_string(),
                    message: format!(
                        "`{}` requires `{expr}`; expected `{}` at position {i}, found {}",
                        node.kind,
                        term.describe(),
                        node.content
                            .get(i)
                            .map(|n| format!("`{}`", n.kind))
                            .unwrap_or_else(|| "end of content".into()),
                    ),
                });
                return;
            }
        }

        if i < node.content.len() {
            out.push(Violation {
                path: path.to_string(),
                message: format!(
                    "`{}` requires `{expr}`; unexpected trailing `{}`",
                    node.kind, node.content[i].kind
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Mark, Node, Schema};

    fn para(text: &str) -> Node {
        Node::element("paragraph", vec![Node::text(text, vec![])])
    }

    #[test]
    fn accepts_a_well_formed_document() {
        let doc = Node::element(
            "doc",
            vec![
                Node::element("heading", vec![Node::text("T", vec![])])
                    .with_attr("level", 1.into()),
                para("body"),
                Node::element(
                    "bulletList",
                    vec![Node::element("listItem", vec![para("item")])],
                ),
            ],
        );
        assert_eq!(Schema::v0().validate(&doc), Ok(()));
    }

    /// The exact shape that masqueraded as a serializer bug during step 1.
    #[test]
    fn rejects_list_item_without_leading_paragraph() {
        let doc = Node::element(
            "doc",
            vec![Node::element(
                "bulletList",
                vec![Node::element(
                    "listItem",
                    vec![Node::element("horizontalRule", vec![])],
                )],
            )],
        );
        let errs = Schema::v0().validate(&doc).unwrap_err();
        assert!(
            errs[0].message.contains("paragraph"),
            "expected a paragraph complaint, got: {}",
            errs[0]
        );
    }

    /// A group is a set. Comparing the whole string excluded every list from
    /// `block+`, which only surfaced once the schema came from TipTap.
    #[test]
    fn membership_of_a_multi_group_node() {
        let doc = Node::element(
            "doc",
            vec![Node::element(
                "bulletList",
                vec![Node::element("listItem", vec![para("x")])],
            )],
        );
        assert_eq!(Schema::v0().validate(&doc), Ok(()));
        assert_eq!(
            Schema::v0().node("bulletList").unwrap().group.as_deref(),
            Some("block list")
        );
    }

    #[test]
    fn rejects_unknown_types_and_misplaced_marks() {
        let doc = Node::element("doc", vec![Node::element("toggle", vec![])]);
        assert!(Schema::v0().validate(&doc).is_err());

        let mut p = para("x");
        p.marks.push(Mark::new("bold"));
        let doc = Node::element("doc", vec![p]);
        let errs = Schema::v0().validate(&doc).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("carries marks")));
    }

    /// Pins the serde trap that made every nullable attribute look required.
    #[test]
    fn explicit_null_default_is_not_a_required_attr() {
        let spec = Schema::v0().node("codeBlock").unwrap();
        assert!(
            spec.attrs["language"].default.is_some(),
            "`\"default\": null` must read as present, not as absent"
        );
        let doc = Node::element(
            "doc",
            vec![Node::element("codeBlock", vec![Node::text("x", vec![])])],
        );
        assert_eq!(Schema::v0().validate(&doc), Ok(()));
    }

    /// TipTap declares `href` with a null default, so an href-less link is
    /// schema-*valid* and validation cannot catch it. Recorded rather than
    /// worked around: the schema comes from the editor (M2.2), and the markdown
    /// projection cannot produce one anyway — a parsed link always carries its
    /// destination, even if empty.
    #[test]
    fn link_href_is_optional_because_tiptap_says_so() {
        assert!(
            Schema::v0().mark("link").unwrap().attrs["href"]
                .default
                .is_some(),
            "`href` has a default, so it is optional"
        );
        let doc = Node::element(
            "doc",
            vec![Node::element(
                "paragraph",
                vec![Node::text("x", vec![Mark::new("link")])],
            )],
        );
        assert_eq!(Schema::v0().validate(&doc), Ok(()));
    }
}
