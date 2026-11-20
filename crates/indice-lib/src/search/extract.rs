//! Pulling title, body, description, headings, keywords, author and lang out
//! of an HTML response.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use html5ever::tendril::StrTendril;
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};

/// Text extracted from an HTML page for indexing.
#[derive(Debug, Default, PartialEq)]
pub struct HtmlText {
    pub title: String,
    pub body: String,
    /// `<meta name=description>` or `og:description`, if present.
    pub description: String,
    /// Concatenated `<h1>`/`<h2>` heading text.
    pub headings: String,
    /// `<meta name=keywords>` content, if present.
    pub keywords: String,
    /// Page author: `<meta name=author>`, falling back to `article:author`.
    pub author: String,
    /// The `<html lang>` attribute value, if present (e.g. `en`, `en-US`).
    pub lang: String,
    /// The page's social-preview image URL: `<meta property=og:image>`, falling
    /// back to `twitter:image`. Used as the crawl's representative thumbnail. May
    /// be relative (resolved against the page URL by the caller).
    pub og_image: String,
}

/// Extract title, body text, description, and headings from raw HTML bytes,
/// skipping script/style content.
pub fn extract_html_text(html: &[u8]) -> HtmlText {
    read_html(html, false).text
}

/// What indice reads from an HTML page: its text, and the `<img>` sources the
/// thumbnail picker looks at.
pub(crate) struct HtmlPage {
    pub text: HtmlText,
    /// Each `<img>`'s `src`, or the first URL of its `srcset` without one, in
    /// page order and as written. Empty unless asked for.
    pub images: Vec<String>,
}

/// How much of a page [`read_html`] hands the parser at a time.
pub(crate) const READ_CHUNK: usize = 16 * 1024;

/// The most nodes the parser may hold at once (the elements open, and those
/// it keeps to reopen, like a `<b>` left open across a `<p>`) before
/// [`read_html`] stops reading the page. Chrome's parser stops nesting at the
/// same depth. Each costs about 120 bytes, and the parser looks through the
/// open elements on many tags, so time grows with depth: 50 MiB of stray
/// `</p>` takes 8 s at 256 deep and 100 s at 4,000. Of 41,148 real pages,
/// the most any held was 196.
pub(crate) const MAX_OPEN: usize = 512;

/// Read `html` in one pass, without building the page's tree.
///
/// html5ever's tree builder applies HTML's parsing rules (an implied `<body>`,
/// which elements hold raw text, `<image>` as `<img>`) and tells [`Reader`]
/// about each element and run of text as it goes. `Reader` keeps a few flags
/// per element and drops the rest, so memory grows with what it extracts and
/// how deeply the page nests, not with how much markup the page has. Past
/// [`MAX_OPEN`] open elements it stops, and the page's text is what it read
/// until then.
pub(crate) fn read_html(html: &[u8], images: bool) -> HtmlPage {
    use html5ever::tendril::{ByteTendril, TendrilSink};
    let reader = Reader::new(images);
    let live = reader.live.clone();
    let mut parser = html5ever::parse_document(reader, Default::default()).from_utf8();
    for chunk in html.chunks(READ_CHUNK) {
        if live.get() > MAX_OPEN {
            break;
        }
        parser.process(ByteTendril::from_slice(chunk));
    }
    parser.finish()
}

/// The `<meta>` values indice reads, as the attribute and value that pick one
/// out. The first `<meta>` matching each is the one used.
const METAS: [(&str, &str); 7] = [
    ("name", "description"),
    ("property", "og:description"),
    ("property", "og:image"),
    ("name", "twitter:image"),
    ("name", "keywords"),
    ("name", "author"),
    ("property", "article:author"),
];

/// What an element's text counts towards. It comes from the element's
/// ancestors, so it's known as soon as the element is added to the page.
#[derive(Clone, Copy, Default)]
struct Context {
    /// Inside `<body>`.
    body: bool,
    /// Inside a `<script>`, `<style>` or `<noscript>`, whose text isn't body
    /// text.
    skip: bool,
    /// Inside the page's first `<title>`.
    title: bool,
    /// Inside an `<h1>` or `<h2>`: its index in `Out::headings`.
    heading: Option<usize>,
}

/// An element, comment or the document, as [`Reader`] keeps it. The tree
/// builder holds these while they are open, and they are freed once it lets
/// go of them.
struct Node {
    /// How many nodes are alive, this one among them.
    live: Rc<Cell<usize>>,
    name: html5ever::QualName,
    /// This is the page's first `<title>`.
    first_title: bool,
    /// The heading this `<h1>` or `<h2>` starts.
    heading: Option<usize>,
    /// The context of what this node was added to.
    outer: Cell<Context>,
    /// The context of what is added to this node.
    inner: Cell<Context>,
    /// Whether this node is in the page, so that
    /// [`TreeSink::append_based_on_parent_node`] can tell.
    attached: Cell<bool>,
}

impl Drop for Node {
    fn drop(&mut self) {
        self.live.set(self.live.get() - 1);
    }
}

impl Node {
    fn new(
        live: &Rc<Cell<usize>>,
        name: html5ever::QualName,
        first_title: bool,
        heading: Option<usize>,
    ) -> Rc<Node> {
        live.set(live.get() + 1);
        Rc::new(Node {
            live: live.clone(),
            name,
            first_title,
            heading,
            outer: Cell::default(),
            inner: Cell::default(),
            attached: Cell::new(false),
        })
    }

    /// Add this node to something whose children have context `outer`.
    fn attach(&self, outer: Context) {
        let local = &*self.name.local;
        self.outer.set(outer);
        self.inner.set(Context {
            body: outer.body || local == "body",
            skip: outer.skip || matches!(local, "script" | "style" | "noscript"),
            title: outer.title || self.first_title,
            heading: self.heading.or(outer.heading),
        });
        self.attached.set(true);
    }
}

/// The text gathered so far.
#[derive(Default)]
struct Out {
    title: String,
    headings: Vec<String>,
    body: String,
    /// Where the run of text being added to the body goes: into a node, or
    /// (`true`) before it. The tree builder hands text over in pieces (it
    /// splits at character references like `&amp;`, for one), and a run is
    /// trimmed as a whole.
    run: Option<(Rc<Node>, bool)>,
    /// Where the current run starts in `body`, and where the space before it
    /// does.
    run_start: usize,
    space_at: usize,
    /// Whether a `<title>` has been seen.
    title_seen: bool,
    /// Whether the page's `<html>` element has been made.
    html_seen: bool,
    /// Its `lang`, if it has one.
    lang: Option<String>,
    /// The `content` of the first `<meta>` matching each of [`METAS`].
    metas: [Option<String>; METAS.len()],
    /// Which of [`METAS`] a `<meta>` has matched.
    metas_seen: [bool; METAS.len()],
    images: Vec<String>,
}

impl Out {
    /// Trim the current run where it is in the body, or take it out, with
    /// the space before it, if it's only whitespace.
    fn end_run(&mut self) {
        if self.run.take().is_none() {
            return;
        }
        let run = &self.body[self.run_start..];
        let lead = run.len() - run.trim_start().len();
        let kept = run.trim().len();
        if kept == 0 {
            self.body.truncate(self.space_at);
        } else {
            self.body.truncate(self.run_start + lead + kept);
            self.body.drain(self.run_start..self.run_start + lead);
        }
    }

    /// Add `text`, going into `to` or (`before`) ahead of it, to whatever
    /// `context` says it counts towards.
    fn text(&mut self, to: &Rc<Node>, before: bool, context: Context, text: &str) {
        if context.title {
            self.title.push_str(text);
        }
        if let Some(i) = context.heading {
            self.headings[i].push_str(text);
        }
        if context.body && !context.skip {
            let same = matches!(&self.run, Some((node, b)) if Rc::ptr_eq(node, to) && *b == before);
            if !same {
                self.end_run();
                self.space_at = self.body.len();
                if !self.body.is_empty() {
                    self.body.push(' ');
                }
                self.run_start = self.body.len();
                self.run = Some((to.clone(), before));
            }
            self.body.push_str(text);
        }
    }
}

/// The [`TreeSink`] behind [`read_html`]. The tree builder calls it through
/// `&self`, so what it gathers sits in a `RefCell`.
struct Reader {
    /// Whether to gather `<img>` sources.
    images: bool,
    live: Rc<Cell<usize>>,
    document: Rc<Node>,
    out: RefCell<Out>,
}

impl Reader {
    fn new(images: bool) -> Reader {
        let live = Rc::default();
        let document = Node::new(&live, Reader::no_name(), false, None);
        document.attached.set(true);
        Reader {
            images,
            live,
            document,
            out: RefCell::default(),
        }
    }

    /// The name of the document, and of comments.
    fn no_name() -> html5ever::QualName {
        html5ever::QualName::new(None, html5ever::ns!(), html5ever::LocalName::from(""))
    }

    fn add(&self, outer: Context, child: NodeOrText<Rc<Node>>, to: &Rc<Node>, before: bool) {
        let mut out = self.out.borrow_mut();
        match child {
            NodeOrText::AppendNode(node) => {
                out.end_run();
                node.attach(outer);
            }
            NodeOrText::AppendText(text) => out.text(to, before, outer, &text),
        }
    }
}

/// The value of the attribute `name` in `attrs`.
fn attr<'a>(attrs: &'a [html5ever::Attribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| &*a.name.local == name)
        .map(|a| &*a.value)
}

impl TreeSink for Reader {
    type Handle = Rc<Node>;
    type Output = HtmlPage;
    type ElemName<'a> = &'a html5ever::QualName;

    fn finish(self) -> HtmlPage {
        let mut out = self.out.into_inner();
        out.end_run();
        let [description, og_description, og_image, twitter_image, keywords, author, article_author] =
            out.metas;
        HtmlPage {
            text: HtmlText {
                title: out.title.trim().to_string(),
                body: out.body,
                description: description.or(og_description).unwrap_or_default(),
                headings: out
                    .headings
                    .iter()
                    .map(|h| h.trim())
                    .filter(|h| !h.is_empty())
                    .collect::<Vec<_>>()
                    .join(" "),
                keywords: keywords.unwrap_or_default(),
                author: author.or(article_author).unwrap_or_default(),
                lang: out.lang.as_deref().unwrap_or("").trim().to_string(),
                og_image: og_image.or(twitter_image).unwrap_or_default(),
            },
            images: out.images,
        }
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> Rc<Node> {
        self.document.clone()
    }

    fn elem_name<'a>(&'a self, target: &'a Rc<Node>) -> &'a html5ever::QualName {
        &target.name
    }

    fn create_element(
        &self,
        name: html5ever::QualName,
        attrs: Vec<html5ever::Attribute>,
        _flags: ElementFlags,
    ) -> Rc<Node> {
        let mut out = self.out.borrow_mut();
        let mut first_title = false;
        let mut heading = None;
        match &*name.local {
            "html" if !out.html_seen => {
                out.html_seen = true;
                out.lang = attr(&attrs, "lang").map(str::to_string);
            }
            "meta" => {
                let content = attr(&attrs, "content")
                    .map(str::trim)
                    .filter(|c| !c.is_empty());
                for (i, (key, value)) in METAS.iter().enumerate() {
                    if !out.metas_seen[i] && attr(&attrs, key) == Some(value) {
                        out.metas_seen[i] = true;
                        out.metas[i] = content.map(str::to_string);
                    }
                }
            }
            "img" if self.images => {
                let src = attr(&attrs, "src").or_else(|| {
                    // The first candidate URL of a srcset, before its descriptor.
                    attr(&attrs, "srcset")?
                        .split(',')
                        .next()?
                        .split_whitespace()
                        .next()
                });
                if let Some(src) = src {
                    out.images.push(src.to_string());
                }
            }
            "title" if !out.title_seen => {
                out.title_seen = true;
                first_title = true;
            }
            "h1" | "h2" => {
                heading = Some(out.headings.len());
                out.headings.push(String::new());
            }
            _ => {}
        }
        Node::new(&self.live, name, first_title, heading)
    }

    fn create_comment(&self, _text: StrTendril) -> Rc<Node> {
        Node::new(&self.live, Reader::no_name(), false, None)
    }

    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> Rc<Node> {
        Node::new(&self.live, Reader::no_name(), false, None)
    }

    fn append(&self, parent: &Rc<Node>, child: NodeOrText<Rc<Node>>) {
        self.add(parent.inner.get(), child, parent, false);
    }

    fn append_before_sibling(&self, sibling: &Rc<Node>, child: NodeOrText<Rc<Node>>) {
        // What goes before `sibling` shares its parent, and so its context.
        self.add(sibling.outer.get(), child, sibling, true);
    }

    fn append_based_on_parent_node(
        &self,
        element: &Rc<Node>,
        prev_element: &Rc<Node>,
        child: NodeOrText<Rc<Node>>,
    ) {
        if element.attached.get() {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(&self, _: StrTendril, _: StrTendril, _: StrTendril) {}

    fn get_template_contents(&self, target: &Rc<Node>) -> Rc<Node> {
        target.clone()
    }

    fn same_node(&self, x: &Rc<Node>, y: &Rc<Node>) -> bool {
        Rc::ptr_eq(x, y)
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    // An `<html>` tag after the parser has made the page's `<html>`, as when
    // a `<meta>` or a stray script comes first, adds its attributes to it.
    // Of those indice reads only `lang`.
    fn add_attrs_if_missing(&self, target: &Rc<Node>, attrs: Vec<html5ever::Attribute>) {
        let mut out = self.out.borrow_mut();
        if &*target.name.local == "html" && out.lang.is_none() {
            out.lang = attr(&attrs, "lang").map(str::to_string);
        }
    }

    fn remove_from_parent(&self, target: &Rc<Node>) {
        self.out.borrow_mut().end_run();
        target.attached.set(false);
    }

    // The tree builder moves children when it fixes misnested tags like
    // `<b><p>…</b>`. They keep the context they were added with, which is
    // the same in all but contrived pages.
    fn reparent_children(&self, _node: &Rc<Node>, _new_parent: &Rc<Node>) {
        self.out.borrow_mut().end_run();
    }
}

/// At most `max` bytes of `bytes`, cut back to the start of a UTF-8 character
/// so that text cut there doesn't end in a broken one. A character is at most
/// four bytes, so no more than three are dropped, even from bytes that aren't
/// UTF-8 at all.
pub(crate) fn utf8_prefix(bytes: &[u8], max: usize) -> &[u8] {
    if bytes.len() <= max {
        return bytes;
    }
    let mut end = max;
    // A continuation byte (0b10xxxxxx) is never the start of a character.
    while end > max.saturating_sub(3) && end > 0 && bytes[end] & 0xC0 == 0x80 {
        end -= 1;
    }
    &bytes[..end]
}
