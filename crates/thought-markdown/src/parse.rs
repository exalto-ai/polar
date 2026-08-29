//! CommonMark + GFM -> ProseMirror tree.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use thought_schema::{Mark, Node, normalize_font_size};

use crate::TITLE_MARKER;

pub fn from_markdown(md: &str) -> Node {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);

    let mut builder = Builder::default();
    for event in Parser::new_ext(md, opts) {
        builder.event(event);
    }
    builder.finish()
}

/// Group consecutive inline children into paragraphs, leaving blocks alone.
fn wrap_loose_inlines(node: &mut Node) {
    if !node.content.iter().any(Node::is_text) {
        return;
    }
    let mut out: Vec<Node> = Vec::with_capacity(node.content.len());
    let mut run: Vec<Node> = Vec::new();
    for child in node.content.drain(..) {
        if child.is_text() {
            run.push(child);
        } else {
            if !run.is_empty() {
                out.push(Node::element("paragraph", std::mem::take(&mut run)));
            }
            out.push(child);
        }
    }
    if !run.is_empty() {
        out.push(Node::element("paragraph", run));
    }
    node.content = out;
}

#[derive(Default)]
struct Builder {
    /// Open block nodes. The root `doc` sits at the bottom.
    stack: Vec<Node>,
    /// Marks currently applying to text, outermost first.
    marks: Vec<Mark>,
    /// Set while inside a fenced block, where text is literal.
    in_code_block: bool,
    /// GFM marks header cells by position, not by tag.
    in_table_head: bool,
    /// Every raw HTML span, including spans this projection ignores. Keeping
    /// the nesting prevents an ignored inner span from closing an outer
    /// font-size span early.
    html_spans: Vec<bool>,
    /// Recognized font-size spans. The last entry is the active size, so
    /// nested spans temporarily override and then restore their parent.
    font_sizes: Vec<String>,
    /// One-shot metadata for the immediately following level-one heading.
    pending_title: bool,
}

impl Builder {
    fn top(&mut self) -> &mut Node {
        if self.stack.is_empty() {
            self.stack.push(Node::element("doc", vec![]));
        }
        self.stack.last_mut().expect("stack is non-empty")
    }

    fn open(&mut self, node: Node) {
        if self.stack.is_empty() {
            self.stack.push(Node::element("doc", vec![]));
        }
        self.stack.push(node);
    }

    fn close(&mut self) {
        if self.stack.len() > 1 {
            let node = self.stack.pop().expect("checked len > 1");
            self.top().content.push(node);
        }
    }

    fn push_text(&mut self, text: &str) {
        let marks = self.active_marks();
        self.top().content.push(Node::text(text, marks));
    }

    fn active_marks(&self) -> Vec<Mark> {
        let mut marks = self.marks.clone();
        if let Some(size) = self.font_sizes.last() {
            marks.push(Mark::new("fontSize").with_attr("size", size.clone().into()));
        }
        marks
    }

    fn inline_html(&mut self, html: &str) {
        if is_closing_span(html) {
            if self.html_spans.pop() == Some(true) {
                self.font_sizes.pop();
            }
            return;
        }

        if !is_opening_span(html) {
            return;
        }

        match opening_font_size(html) {
            Some(size) => {
                self.html_spans.push(true);
                self.font_sizes.push(size);
            }
            None => self.html_spans.push(false),
        }
    }

    fn block_html(&mut self, html: &str) {
        // Recognize only our exact marker. Arbitrary HTML remains outside the
        // document schema and also cancels a stale or hand-written marker.
        self.pending_title = html.trim() == TITLE_MARKER;
    }

    /// Raw inline HTML cannot safely carry a mark across a block boundary.
    /// The serializer always closes its spans, while this reset keeps malformed
    /// hand-written Markdown from styling unrelated later blocks.
    fn clear_inline_html(&mut self) {
        self.html_spans.clear();
        self.font_sizes.clear();
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),

            Event::Text(t) => self.push_text(&t),

            // Inline `code` arrives as one event, not a mark span.
            Event::Code(t) => {
                let mut marks = self.active_marks();
                marks.push(Mark::new("code"));
                self.top().content.push(Node::text(t.to_string(), marks));
            }

            Event::SoftBreak | Event::HardBreak => self.push_text(" "),
            Event::Rule => {
                self.pending_title = false;
                let hr = Node::element("horizontalRule", vec![]);
                self.top().content.push(hr);
            }

            // Not in the v0 schema (AD-12). Dropping is deliberate: anything
            // that cannot round-trip must not enter the tree.
            Event::InlineHtml(html) => self.inline_html(&html),
            Event::Html(html) => self.block_html(&html),
            Event::FootnoteReference(_)
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => self.pending_title = false,
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        let pending_title = self.pending_title;
        self.pending_title = false;
        match tag {
            Tag::Paragraph => self.open(Node::element("paragraph", vec![])),

            Tag::Heading { level, .. } => {
                let level = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                let mut heading = Node::element("heading", vec![]).with_attr("level", level.into());
                if pending_title && level == 1 {
                    heading = heading.with_attr("variant", "title".into());
                }
                self.open(heading);
            }

            Tag::BlockQuote(_) => self.open(Node::element("blockquote", vec![])),

            Tag::CodeBlock(kind) => {
                self.in_code_block = true;
                let mut node = Node::element("codeBlock", vec![]);
                if let CodeBlockKind::Fenced(lang) = kind
                    && !lang.is_empty()
                {
                    node = node.with_attr("language", lang.to_string().into());
                }
                self.open(node);
            }

            Tag::List(start) => {
                let node = match start {
                    Some(n) => {
                        Node::element("orderedList", vec![]).with_attr("start", (n as i64).into())
                    }
                    None => Node::element("bulletList", vec![]),
                };
                self.open(node);
            }
            Tag::Item => self.open(Node::element("listItem", vec![])),

            Tag::Table(_) => self.open(Node::element("table", vec![])),
            Tag::TableHead => {
                self.in_table_head = true;
                self.open(Node::element("tableRow", vec![]));
            }
            Tag::TableRow => self.open(Node::element("tableRow", vec![])),
            Tag::TableCell => {
                // GFM marks header cells by position; ProseMirror gives them
                // their own node type, which is what TipTap expects.
                let kind = if self.in_table_head {
                    "tableHeader"
                } else {
                    "tableCell"
                };
                self.open(Node::element(kind, vec![]));
            }

            Tag::Strong => self.marks.push(Mark::new("bold")),
            Tag::Emphasis => self.marks.push(Mark::new("italic")),
            Tag::Strikethrough => self.marks.push(Mark::new("strike")),

            Tag::Link {
                dest_url, title, ..
            } => {
                let mut mark = Mark::new("link").with_attr("href", dest_url.to_string().into());
                if !title.is_empty() {
                    mark = mark.with_attr("title", title.to_string().into());
                }
                self.marks.push(mark);
            }

            // Outside the v0 schema; consume without opening a node.
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        if matches!(&tag, TagEnd::HtmlBlock) {
            // pulldown-cmark balances block HTML around Event::Html. The exact
            // title marker is set inside that pair and must reach the heading
            // that follows it.
            return;
        }
        // An end event after the standalone marker means a container or block
        // boundary intervened before the next heading. Do not let metadata
        // escape a blockquote, list item, table cell, or other parent.
        self.pending_title = false;
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => {
                self.clear_inline_html();
                self.close();
            }

            TagEnd::BlockQuote(_) | TagEnd::List(_) => {
                self.clear_inline_html();
                self.close();
            }

            TagEnd::Item => {
                // A *tight* list emits Item -> Text with no Paragraph between,
                // so inline content arrives as a direct child of the item. The
                // schema says listItem holds blocks, so wrap the loose runs.
                self.clear_inline_html();
                if let Some(item) = self.stack.last_mut() {
                    wrap_loose_inlines(item);
                }
                self.close();
            }

            TagEnd::CodeBlock => {
                self.in_code_block = false;
                // The fence's trailing newline is structure, not content.
                if let Some(last) = self.top().content.last_mut()
                    && let Some(text) = last.text.as_mut()
                    && text.ends_with('\n')
                {
                    text.pop();
                }
                self.close();
            }

            TagEnd::Table | TagEnd::TableRow => self.close(),

            TagEnd::TableCell => {
                // ProseMirror table cells hold blocks, not inline content, so
                // GFM's inline cell text is wrapped the way a tight list item's
                // is.
                self.clear_inline_html();
                if let Some(cell) = self.stack.last_mut() {
                    wrap_loose_inlines(cell);
                }
                self.close();
            }
            TagEnd::TableHead => {
                self.in_table_head = false;
                self.close();
            }

            TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link => {
                self.marks.pop();
            }

            _ => {}
        }
    }

    fn finish(mut self) -> Node {
        self.clear_inline_html();
        while self.stack.len() > 1 {
            self.close();
        }
        self.stack
            .pop()
            .unwrap_or_else(|| Node::element("doc", vec![]))
    }
}

/// The serializer emits one exact spelling. Accepting the same shape here is
/// enough for round trips and intentionally avoids treating arbitrary CSS as
/// document data.
fn opening_font_size(html: &str) -> Option<String> {
    let value = html
        .trim()
        .strip_prefix("<span style=\"font-size: ")?
        .strip_suffix("\">")?;
    normalize_font_size(value)
}

fn is_opening_span(html: &str) -> bool {
    let html = html.trim();
    if html.ends_with("/>") {
        return false;
    }
    let lower = html.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("<span") else {
        return false;
    };
    rest.starts_with('>') || rest.chars().next().is_some_and(char::is_whitespace)
}

fn is_closing_span(html: &str) -> bool {
    html.trim().eq_ignore_ascii_case("</span>")
}
