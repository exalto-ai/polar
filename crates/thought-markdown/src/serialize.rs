//! ProseMirror tree -> CommonMark + GFM.

use thought_schema::{Mark, Node, normalize_font_size};

use crate::TITLE_MARKER;

pub fn to_markdown(doc: &Node) -> String {
    to_markdown_with_spans(doc).0
}

/// Markdown plus the 1-based inclusive line range of each top-level block.
///
/// Anchors travel *beside* the text rather than inside it: agents get clean
/// markdown, and block identity is carried out of band. Embedding ids in the
/// markdown itself would pollute every export and invite agents to edit them.
pub fn to_markdown_with_spans(doc: &Node) -> (String, Vec<(usize, usize)>) {
    let mut out = String::new();
    let mut spans = Vec::with_capacity(doc.content.len());
    blocks_tracked(&doc.content, &mut out, Some(&mut spans));
    while out.ends_with('\n') {
        out.pop();
    }

    // Clamp only now, because only now is the real line count known: a block
    // can serialize to nothing — an empty paragraph does — and the trailing
    // newlines just stripped were counted while walking. Left alone, the last
    // block claims the line *after* the document, and an agent asking for line
    // 12 of an 11-line document gets an answer rather than an error.
    let total = out.lines().count().max(1);
    for span in &mut spans {
        span.0 = span.0.clamp(1, total);
        span.1 = span.1.clamp(span.0, total);
    }

    (out, spans)
}

fn line_count(s: &str) -> usize {
    s.bytes().filter(|b| *b == b'\n').count()
}

fn blocks(nodes: &[Node], out: &mut String) {
    blocks_tracked(nodes, out, None);
}

fn blocks_tracked(nodes: &[Node], out: &mut String, mut spans: Option<&mut Vec<(usize, usize)>>) {
    // Two adjacent lists sharing a marker merge into one on re-parse, which
    // silently destroys document structure. CommonMark starts a new list when
    // the marker character changes, so alternate between siblings.
    let mut prev_bullet: Option<char> = None;
    let mut prev_ordered: Option<char> = None;

    for (i, node) in nodes.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let start_line = line_count(out) + 1;
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
        if let Some(spans) = spans.as_deref_mut() {
            spans.push((start_line, line_count(out).max(start_line)));
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
            if level == 1 && node.attr_str("variant") == Some("title") {
                out.push_str(TITLE_MARKER);
                out.push('\n');
            }
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
            let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
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
        "table" => table(node, out),
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

/// GFM pipe table. The format requires a header row and a delimiter row, so a
/// table's first row is emitted as the header whatever its cells are — a fact
/// the round-trip property holds us to.
///
/// `colspan` and `rowspan` are not expressible in GFM. A merged cell is
/// therefore projection-only under AD-12: it survives in the document and the
/// editor, but markdown flattens it.
fn table(node: &Node, out: &mut String) {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for row in &node.content {
        rows.push(
            row.content
                .iter()
                .map(|cell| {
                    // A cell holds blocks; GFM gives it one line. Concatenate
                    // their inline content rather than dropping all but the
                    // first, which would lose text silently.
                    let text: String = cell.content.iter().map(|b| inlines(&b.content)).collect();
                    text.replace('|', "\\|")
                })
                .collect(),
        );
    }
    if rows.is_empty() {
        return;
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);

    for (i, row) in rows.iter().enumerate() {
        out.push('|');
        for c in 0..width {
            out.push(' ');
            out.push_str(row.get(c).map(String::as_str).unwrap_or(""));
            out.push_str(" |");
        }
        out.push('\n');
        if i == 0 {
            out.push('|');
            for _ in 0..width {
                out.push_str(" --- |");
            }
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
    let mut s = if has_code {
        text.to_string()
    } else {
        escape(text)
    };

    // Innermost first, so the canonical mark order produces stable nesting.
    for mark in node.marks.iter().rev() {
        s = wrap(&s, mark);
    }
    s
}

fn wrap(s: &str, mark: &Mark) -> String {
    match mark.kind.as_str() {
        // Names follow TipTap, which defines the schema (M2.2). ProseMirror's
        // own convention is strong/em; the editor is the harder thing to
        // change, so Rust follows it.
        "bold" => format!("**{s}**"),
        "italic" => format!("*{s}*"),
        "strike" => format!("~~{s}~~"),
        "fontSize" => mark
            .attrs
            .get("size")
            .and_then(|value| value.as_str())
            .and_then(normalize_font_size)
            .map_or_else(
                || s.to_string(),
                |size| format!("<span style=\"font-size: {size}\">{s}</span>"),
            ),
        "code" => {
            let longest = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
            let fence = "`".repeat(longest + 1);
            let pad = if s.starts_with('`') || s.ends_with('`') {
                " "
            } else {
                ""
            };
            format!("{fence}{pad}{s}{pad}{fence}")
        }
        "link" => {
            let href = mark
                .attrs
                .get("href")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let destination = link_destination(href);
            match mark.attrs.get("title").and_then(|v| v.as_str()) {
                Some(title) => format!("[{s}]({destination} \"{}\")", link_title(title)),
                None => format!("[{s}]({destination})"),
            }
        }
        _ => s.to_string(),
    }
}

/// Angle-bracket destinations keep spaces and balanced punctuation out of the
/// Markdown parser's grammar. Escape only the characters that terminate that
/// form so the parser reconstructs the exact href.
fn link_destination(href: &str) -> String {
    let mut out = String::with_capacity(href.len() + 2);
    out.push('<');
    for character in href.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '\\' | '<' | '>' => {
                out.push('\\');
                out.push(character);
            }
            _ => out.push(character),
        }
    }
    out.push('>');
    out
}

fn link_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for character in title.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '\\' | '"' => {
                out.push('\\');
                out.push(character);
            }
            _ => out.push(character),
        }
    }
    out
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
