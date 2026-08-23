//! ProseMirror tree -> CommonMark + GFM.

use polar_schema::{Mark, Node};

pub fn to_markdown(doc: &Node) -> String {
    let mut out = String::new();
    blocks(&doc.content, &mut out);
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

fn blocks(nodes: &[Node], out: &mut String) {
    // Two adjacent lists sharing a marker merge into one on re-parse, which
    // silently destroys document structure. CommonMark starts a new list when
    // the marker character changes, so alternate between siblings.
    let mut prev_bullet: Option<char> = None;
    let mut prev_ordered: Option<char> = None;

    for (i, node) in nodes.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        match node.kind.as_str() {
            "bulletList" => {
                let marker = if prev_bullet == Some('-') { '*' } else { '-' };
                prev_bullet = Some(marker);
                prev_ordered = None;
                list(node, None, marker, out);
            }
            "orderedList" => {
                let delim = if prev_ordered == Some('.') { ')' } else { '.' };
                prev_ordered = Some(delim);
                prev_bullet = None;
                list(node, Some(node.attr_i64("start").unwrap_or(1)), delim, out);
            }
            _ => {
                prev_bullet = None;
                prev_ordered = None;
                block(node, out);
            }
        }
    }
}

fn block(node: &Node, out: &mut String) {
    match node.kind.as_str() {
        "paragraph" => {
            out.push_str(&inlines(&node.content));
            out.push('\n');
        }
        "heading" => {
            let level = node.attr_i64("level").unwrap_or(1).clamp(1, 6) as usize;
            out.push_str(&"#".repeat(level));
            out.push(' ');
            let mut text = inlines(&node.content);
            // An ATX heading strips a trailing run of `#`, reading it as a
            // closing sequence. Escape only the final one — escaping every `#`
            // in the document would make the projection noisy to read, and
            // agents read this.
            // `escape` may already have escaped it (a `#` that also began the
            // text); escaping twice emits a literal backslash.
            if text.ends_with('#') && !text.ends_with("\\#") {
                text.pop();
                text.push_str("\\#");
            }
            out.push_str(&text);
            out.push('\n');
        }
        "blockquote" => {
            let mut inner = String::new();
            blocks(&node.content, &mut inner);
            for line in inner.trim_end_matches('\n').split('\n') {
                if line.is_empty() {
                    out.push_str(">\n");
                } else {
                    out.push_str("> ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        "codeBlock" => {
            let text: String = node.content.iter().filter_map(|n| n.text.clone()).collect();
            // A fence must be longer than any backtick run inside it, or the
            // block terminates early and the round-trip silently loses content.
            let longest = text
                .split(|c| c != '`')
                .map(str::len)
                .max()
                .unwrap_or(0);
            let fence = "`".repeat(longest.max(2) + 1);
            out.push_str(&fence);
            if let Some(lang) = node.attr_str("language") {
                out.push_str(lang);
            }
            out.push('\n');
            out.push_str(&text);
            // Always terminate, even when the text already ends in a newline.
            // The parser strips exactly one; making this conditional loses the
            // final newline of any block that has one.
            out.push('\n');
            out.push_str(&fence);
            out.push('\n');
        }
        // bulletList / orderedList are handled in `blocks`, which alone can
        // see a list's siblings and pick a non-merging marker.
        // `***`, not `---`: a dash rule doubles as a setext underline and
        // fuses with a `-` list marker (`- ---` re-parses as a rule at the
        // outer level). Asterisks are unambiguous in both positions.
        "horizontalRule" => out.push_str("***\n"),
        _ => {
            // Unknown block: emit its inline content rather than dropping it.
            out.push_str(&inlines(&node.content));
            out.push('\n');
        }
    }
}

fn list(node: &Node, ordered: Option<i64>, marker_char: char, out: &mut String) {
    for (i, item) in node.content.iter().enumerate() {
        let marker = match ordered {
            Some(start) => format!("{}{} ", start + i as i64, marker_char),
            None => format!("{marker_char} "),
        };
        let indent = " ".repeat(marker.len());

        let mut inner = String::new();
        blocks(&item.content, &mut inner);

        for (j, line) in inner.trim_end_matches('\n').split('\n').enumerate() {
            if j == 0 {
                out.push_str(&marker);
            } else if !line.is_empty() {
                out.push_str(&indent);
            }
            out.push_str(line);
            out.push('\n');
        }
    }
}

fn inlines(nodes: &[Node]) -> String {
    nodes.iter().map(inline).collect()
}

fn inline(node: &Node) -> String {
    let Some(text) = node.text.as_deref() else {
        return String::new();
    };

    // `code` is literal: escaping inside it would round-trip as backslashes.
    let has_code = node.marks.iter().any(|m| m.kind == "code");
    let mut s = if has_code { text.to_string() } else { escape(text) };

    // Innermost first, so the canonical mark order produces stable nesting.
    for mark in node.marks.iter().rev() {
        s = wrap(&s, mark);
    }
    s
}

fn wrap(s: &str, mark: &Mark) -> String {
    match mark.kind.as_str() {
        "strong" => format!("**{s}**"),
        "em" => format!("*{s}*"),
        "strike" => format!("~~{s}~~"),
        "code" => {
            let longest = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
            let fence = "`".repeat(longest + 1);
            let pad = if s.starts_with('`') || s.ends_with('`') { " " } else { "" };
            format!("{fence}{pad}{s}{pad}{fence}")
        }
        "link" => {
            let href = mark.attrs.get("href").and_then(|v| v.as_str()).unwrap_or("");
            match mark.attrs.get("title").and_then(|v| v.as_str()) {
                Some(t) => format!("[{s}]({href} \"{t}\")"),
                None => format!("[{s}]({href})"),
            }
        }
        _ => s.to_string(),
    }
}

/// Escape anything that would otherwise re-parse as structure. Conservative on
/// purpose: an unnecessary backslash is invisible to the reader, a missing one
/// silently changes the tree.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, ch) in text.chars().enumerate() {
        let at_start = i == 0;
        match ch {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '&' | '~' => {
                out.push('\\');
                out.push(ch);
            }
            '#' | '>' | '-' | '+' if at_start => {
                out.push('\\');
                out.push(ch);
            }
            '\n' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}
