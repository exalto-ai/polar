//! CommonMark + GFM -> ProseMirror tree.

use polar_schema::{Mark, Node};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

pub fn from_markdown(md: &str) -> Node {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);

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
        let marks = self.marks.clone();
        self.top().content.push(Node::text(text, marks));
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),

            Event::Text(t) => self.push_text(&t),

            // Inline `code` arrives as one event, not a mark span.
            Event::Code(t) => {
                let mut marks = self.marks.clone();
                marks.push(Mark::new("code"));
                self.top().content.push(Node::text(t.to_string(), marks));
            }

            Event::SoftBreak | Event::HardBreak => self.push_text(" "),
            Event::Rule => {
                let hr = Node::element("horizontalRule", vec![]);
                self.top().content.push(hr);
            }

            // Not in the v0 schema (AD-12). Dropping is deliberate: anything
            // that cannot round-trip must not enter the tree.
            Event::Html(_)
            | Event::InlineHtml(_)
            | Event::FootnoteReference(_)
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
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
                self.open(
                    Node::element("heading", vec![]).with_attr("level", level.into()),
                );
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
                    Some(n) => Node::element("orderedList", vec![])
                        .with_attr("start", (n as i64).into()),
                    None => Node::element("bulletList", vec![]),
                };
                self.open(node);
            }
            Tag::Item => self.open(Node::element("listItem", vec![])),

            Tag::Strong => self.marks.push(Mark::new("strong")),
            Tag::Emphasis => self.marks.push(Mark::new("em")),
            Tag::Strikethrough => self.marks.push(Mark::new("strike")),

            Tag::Link { dest_url, title, .. } => {
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
        match tag {
            TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::BlockQuote(_)
            | TagEnd::List(_) => self.close(),

            TagEnd::Item => {
                // A *tight* list emits Item -> Text with no Paragraph between,
                // so inline content arrives as a direct child of the item. The
                // schema says listItem holds blocks, so wrap the loose runs.
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

            TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link => {
                self.marks.pop();
            }

            _ => {}
        }
    }

    fn finish(mut self) -> Node {
        while self.stack.len() > 1 {
            self.close();
        }
        self.stack.pop().unwrap_or_else(|| Node::element("doc", vec![]))
    }
}
