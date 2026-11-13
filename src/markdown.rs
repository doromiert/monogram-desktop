//! Monogram mobile's chat Markdown profile, rendered as bounded native blocks.
//! MarkdownParser/MonogramArticles reference: GPL-3.0-or-later.
use crate::formatting::{Range, Style};
use pulldown_cmark::{Event, Options, Parser, Tag};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default)]
pub struct RichText {
    pub text: String,
    pub ranges: Vec<Range>,
    pub links: Vec<Link>,
    pub math: Vec<Formula>,
}
#[derive(Clone, Debug)]
pub struct Link {
    pub start: usize,
    pub end: usize,
    pub uri: String,
}
#[derive(Clone, Debug)]
pub struct Formula {
    pub start: usize,
    pub end: usize,
    pub source: String,
    pub display: bool,
}
#[derive(Clone, Debug)]
pub enum Block {
    Paragraph(RichText),
    Heading(u8, RichText),
    Quote(Vec<Block>),
    Code(String, String),
    List(Option<Vec<String>>, Vec<Vec<Block>>),
    Table(Table),
    Rule,
    Details {
        title: RichText,
        blocks: Vec<Block>,
        open: bool,
    },
    Math(String),
}
#[derive(Clone, Debug)]
pub struct Table {
    pub rows: Vec<Vec<RichText>>,
    pub alignment: Vec<pulldown_cmark::Alignment>,
}
#[derive(Clone, Debug)]
pub struct Document {
    pub blocks: Vec<Block>,
    pub rich: bool,
}
#[derive(Debug)]
enum Node {
    Tag(Tag<'static>, Vec<Node>),
    Leaf(Event<'static>),
}
impl RichText {
    fn append(&mut self, text: &str, styles: &BTreeSet<Style>, uri: Option<&str>) {
        let start = self.text.chars().count();
        self.text.push_str(text);
        let end = self.text.chars().count();
        if start == end {
            return;
        }
        if !styles.is_empty() {
            if let Some(last) = self
                .ranges
                .last_mut()
                .filter(|r| r.end == start && &r.styles == styles)
            {
                last.end = end;
            } else {
                self.ranges.push(Range {
                    start,
                    end,
                    styles: styles.clone(),
                    language: None,
                });
            }
        }
        if let Some(uri) = uri.filter(|s| safe_uri(s)) {
            if let Some(last) = self
                .links
                .last_mut()
                .filter(|l| l.end == start && l.uri == uri)
            {
                last.end = end;
            } else {
                self.links.push(Link {
                    start,
                    end,
                    uri: uri.into(),
                });
            }
        }
    }
    pub fn plain(value: &str) -> Self {
        Self {
            text: value.into(),
            ..Default::default()
        }
    }
    fn formula(&mut self, source: &str, display: bool, styles: &BTreeSet<Style>) {
        let start = self.text.chars().count();
        self.append("\u{fffc}", styles, None);
        self.math.push(Formula {
            start,
            end: start + 1,
            source: source.into(),
            display,
        });
    }
}
pub fn safe_uri(uri: &str) -> bool {
    uri.len() <= 8192
        && !uri.chars().any(|c| c.is_control())
        && (uri.starts_with("https://")
            || uri.starts_with("http://")
            || uri.starts_with("mailto:")
            || uri.starts_with("tel:")
            || uri.starts_with("xmpp:")
            || uri.starts_with('#'))
}
fn ast(source: &str) -> Option<Vec<Node>> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH;
    let mut stack: Vec<(Tag<'static>, Vec<Node>)> = vec![];
    let mut root = vec![];
    let mut count = 0;
    for (mut event, range) in Parser::new_ext(source, options).into_offset_iter() {
        if let Event::InlineMath(value) = &event {
            if value.trim() != value.as_ref()
                || source[range.end..].starts_with(|c: char| c.is_ascii_digit())
            {
                event = Event::Text(format!("${value}$").into());
            }
        }
        count += 1;
        if count > 4096 {
            return None;
        }
        match event {
            Event::Start(tag) => {
                if stack.len() >= 24 {
                    return None;
                }
                stack.push((tag.into_static(), vec![]));
            }
            Event::End(_) => {
                let (tag, children) = stack.pop()?;
                let node = Node::Tag(tag, children);
                if let Some((_, parent)) = stack.last_mut() {
                    parent.push(node);
                } else {
                    root.push(node);
                }
            }
            event => {
                let node = Node::Leaf(event.into_static());
                if let Some((_, parent)) = stack.last_mut() {
                    parent.push(node);
                } else {
                    root.push(node);
                }
            }
        }
    }
    Some(root)
}
/// Match mobile's explicit-space quotes and preserve bare URL punctuation literally.
fn prepare(source: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut fence: Option<char> = None;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let lead = line.len() - trimmed.len();
        if lead <= 3 && (trimmed.starts_with("```") || trimmed.starts_with("~~~")) {
            let c = trimmed.chars().next().unwrap();
            if fence == Some(c) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(c);
            }
            out.push_str(line);
            quoted = false;
            continue;
        }
        if fence.is_some() {
            out.push_str(line);
            continue;
        }
        let quote = lead <= 3 && trimmed.starts_with("> ");
        if quoted && !quote && !line.trim().is_empty() {
            out.push('\n');
        }
        quoted = quote;
        if lead <= 3 && trimmed.starts_with('>') && !quote {
            out.push_str(&line[..lead]);
            out.push('\\');
            out.push_str(trimmed);
            continue;
        }
        let quoted_line = quote.then(|| {
            format!(
                "{}> <span></span>{}{}",
                &line[..lead],
                trimmed[2..].trim_end_matches('\n'),
                if line.ends_with('\n') { "  \n" } else { "" }
            )
        });
        let line = quoted_line.as_deref().unwrap_or(line);
        let mut cursor = 0;
        let mut ticks = false;
        let bytes = line.as_bytes();
        while cursor < line.len() {
            let tail = &line[cursor..];
            if tail.starts_with('`') {
                ticks = !ticks;
            }
            let url = !ticks
                && (tail.starts_with("https://")
                    || tail.starts_with("http://")
                    || tail.starts_with("www."))
                && (cursor == 0
                    || !line[..cursor]
                        .chars()
                        .next_back()
                        .unwrap()
                        .is_alphanumeric())
                && (cursor == 0 || !matches!(bytes[cursor - 1], b'<' | b'('));
            if url {
                let mut end = cursor
                    + tail
                        .find(|c: char| {
                            c.is_whitespace()
                                || matches!(c, '<' | '>' | '`' | '"' | '\'' | '[' | ']')
                        })
                        .unwrap_or(tail.len());
                while end > cursor
                    && matches!(
                        bytes[end - 1],
                        b'.' | b',' | b'!' | b'?' | b':' | b';' | b')'
                    )
                {
                    end -= 1;
                }
                if end > cursor {
                    let raw = &line[cursor..end];
                    if raw.starts_with("www.") {
                        out.push('[');
                        out.push_str(raw);
                        out.push_str("](<https://");
                        out.push_str(raw);
                        out.push_str(">)");
                    } else {
                        out.push('<');
                        out.push_str(raw);
                        out.push('>');
                    }
                    cursor = end;
                    continue;
                }
            }
            let c = tail.chars().next().unwrap();
            out.push(c);
            cursor += c.len_utf8();
        }
    }
    out
}
pub fn parse(source: &str) -> Option<Document> {
    if source.encode_utf16().count() > 32768 {
        return None;
    }
    let mut budget = 2048;
    let mut blocks = profile_blocks(source, 0, &mut budget)?;
    let mut markers = ordered_markers(source);
    for block in &mut blocks {
        if let Block::List(Some(numbers), _) = block {
            for number in numbers {
                if let Some(marker) = markers.pop_front() {
                    *number = marker;
                }
            }
        }
    }
    let rich = blocks.iter().any(block_rich);
    Some(Document { blocks, rich })
}
fn block_rich(block: &Block) -> bool {
    match block {
        Block::Paragraph(t) => {
            !t.ranges.is_empty()
                || !t.math.is_empty()
                || t.links.iter().any(|l| {
                    t.text
                        .chars()
                        .skip(l.start)
                        .take(l.end - l.start)
                        .collect::<String>()
                        != l.uri
                })
        }
        _ => true,
    }
}
fn blocks(nodes: &[Node], depth: usize, budget: &mut usize) -> Option<Vec<Block>> {
    if depth > 24 {
        return None;
    }
    let mut out = vec![];
    for node in nodes {
        *budget = budget.checked_sub(1)?;
        match node {
            Node::Tag(Tag::Paragraph, children) => {
                out.push(Block::Paragraph(rich(children, false)))
            }
            Node::Tag(Tag::Heading { level, .. }, children) => {
                out.push(Block::Heading(*level as u8, rich(children, false)))
            }
            Node::Tag(Tag::BlockQuote(_), children) => {
                out.push(Block::Quote(vec![Block::Paragraph(rich(children, true))]))
            }
            Node::Tag(Tag::CodeBlock(kind), children) => {
                let language = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(v) => v.to_string(),
                    _ => String::new(),
                };
                out.push(Block::Code(rich(children, false).text, language));
            }
            Node::Tag(Tag::List(start), children) => {
                let items = children
                    .iter()
                    .filter_map(|child| {
                        if let Node::Tag(Tag::Item, children) = child {
                            Some(blocks(children, depth + 1, budget))
                        } else {
                            None
                        }
                    })
                    .collect::<Option<Vec<_>>>()?;
                let numbers = start.map(|start| {
                    (0..items.len())
                        .map(|i| (start + i as u64).to_string())
                        .collect()
                });
                out.push(Block::List(numbers, items));
            }
            Node::Tag(Tag::Table(alignment), children) => {
                let mut rows = vec![];
                for child in children {
                    match child {
                        Node::Tag(Tag::TableHead, cells) => rows.push(table_cells(cells)),
                        Node::Tag(Tag::TableRow, cells) => rows.push(table_cells(cells)),
                        _ => {}
                    }
                }
                out.push(Block::Table(Table {
                    rows,
                    alignment: alignment.clone(),
                }));
            }
            Node::Tag(Tag::FootnoteDefinition(id), children) => out.push(Block::Details {
                title: RichText::plain(&format!("Reference {id}")),
                blocks: blocks(children, depth + 1, budget)?,
                open: true,
            }),
            Node::Tag(Tag::HtmlBlock, children) => {
                let raw = children
                    .iter()
                    .filter_map(|c| {
                        if let Node::Leaf(Event::Html(s)) = c {
                            Some(s.as_ref())
                        } else {
                            None
                        }
                    })
                    .collect::<String>();
                if let Some(details) = details(&raw, depth, budget) {
                    out.push(details);
                } else {
                    let value = rich_html(&raw);
                    if !value.text.trim().is_empty() {
                        out.push(Block::Paragraph(value));
                    }
                }
            }
            Node::Leaf(Event::Rule) => out.push(Block::Rule),
            Node::Leaf(Event::DisplayMath(value)) => out.push(Block::Math(value.to_string())),
            Node::Leaf(
                Event::Text(_)
                | Event::Code(_)
                | Event::InlineHtml(_)
                | Event::Html(_)
                | Event::SoftBreak
                | Event::HardBreak
                | Event::TaskListMarker(_)
                | Event::InlineMath(_)
                | Event::FootnoteReference(_),
            ) => {
                if let Some(Block::Paragraph(last)) = out.last_mut() {
                    append_nodes(
                        std::slice::from_ref(node),
                        &mut InlineState::default(),
                        last,
                        false,
                    );
                } else {
                    out.push(Block::Paragraph(rich(std::slice::from_ref(node), false)));
                }
            }
            Node::Tag(_, children) => out.extend(blocks(children, depth + 1, budget)?),
            _ => {}
        }
    }
    Some(out)
}
fn table_cells(nodes: &[Node]) -> Vec<RichText> {
    nodes
        .iter()
        .filter_map(|n| {
            if let Node::Tag(Tag::TableCell, children) = n {
                Some(rich(children, false))
            } else {
                None
            }
        })
        .collect()
}
fn details(raw: &str, depth: usize, budget: &mut usize) -> Option<Block> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("<details") {
        return None;
    }
    let head = trimmed.find('>')?;
    let end = trimmed.rfind("</details>")?;
    let inner = &trimmed[head + 1..end];
    let (title, body) = if let Some(start) = inner.find("<summary>") {
        let stop = inner[start..].find("</summary>")? + start;
        (rich_html(&inner[start + 9..stop]), &inner[stop + 10..])
    } else {
        (RichText::plain("Details"), inner)
    };
    let nodes = ast(body)?;
    Some(Block::Details {
        title,
        blocks: blocks(&nodes, depth + 1, budget)?,
        open: trimmed[..head].split_whitespace().any(|s| s == "open"),
    })
}
#[derive(Clone, Default)]
struct InlineState {
    styles: BTreeSet<Style>,
    link: Option<String>,
    html: Vec<(String, BTreeSet<Style>, Option<String>)>,
}
fn rich(nodes: &[Node], quote: bool) -> RichText {
    let mut text = RichText::default();
    append_nodes(nodes, &mut InlineState::default(), &mut text, quote);
    text
}
fn append_nodes(nodes: &[Node], state: &mut InlineState, text: &mut RichText, quote: bool) {
    for node in nodes {
        match node {
            Node::Tag(tag, children) => {
                let mut inner = state.clone();
                match tag {
                    Tag::Strong => {
                        inner.styles.insert(Style::Strong);
                    }
                    Tag::Emphasis => {
                        inner.styles.insert(Style::Emphasis);
                    }
                    Tag::Strikethrough => {
                        inner.styles.insert(Style::Deleted);
                    }
                    Tag::Link { dest_url, .. } => {
                        inner.link = Some(dest_url.to_string());
                    }
                    _ => {}
                }
                append_nodes(children, &mut inner, text, quote);
            }
            Node::Leaf(Event::Text(s)) => text.append(s, &state.styles, state.link.as_deref()),
            Node::Leaf(Event::Code(s)) => {
                let mut styles = state.styles.clone();
                styles.insert(Style::Code);
                text.append(s, &styles, state.link.as_deref());
            }
            Node::Leaf(Event::InlineHtml(s) | Event::Html(s)) => html(s, state, text),
            Node::Leaf(Event::SoftBreak) => text.append(
                if quote { "\n" } else { " " },
                &state.styles,
                state.link.as_deref(),
            ),
            Node::Leaf(Event::HardBreak) => text.append("\n", &state.styles, state.link.as_deref()),
            Node::Leaf(Event::TaskListMarker(v)) => {
                text.append(if *v { "☑ " } else { "☐ " }, &state.styles, None)
            }
            Node::Leaf(Event::InlineMath(s)) => {
                if s.trim() != s.as_ref() {
                    text.append(&format!("${s}$"), &state.styles, state.link.as_deref());
                } else {
                    text.formula(s, false, &state.styles);
                }
            }
            Node::Leaf(Event::DisplayMath(s)) => text.formula(s, true, &state.styles),
            Node::Leaf(Event::FootnoteReference(id)) => {
                let mut styles = state.styles.clone();
                styles.insert(Style::Sup);
                text.append(&format!("[{id}]"), &styles, Some(&format!("#fn-{id}")));
            }
            _ => {}
        }
    }
}
fn rich_html(source: &str) -> RichText {
    let mut text = RichText::default();
    html(source, &mut InlineState::default(), &mut text);
    text
}
fn html(source: &str, state: &mut InlineState, out: &mut RichText) {
    let mut at = 0;
    while at < source.len() {
        let Some(relative) = source[at..].find('<') else {
            out.append(
                &entities(&source[at..]),
                &state.styles,
                state.link.as_deref(),
            );
            break;
        };
        let start = at + relative;
        out.append(
            &entities(&source[at..start]),
            &state.styles,
            state.link.as_deref(),
        );
        let Some(close) = source[start..].find('>') else {
            out.append(&source[start..], &state.styles, state.link.as_deref());
            break;
        };
        let end = start + close + 1;
        let inside = source[start + 1..end - 1].trim();
        let closing = inside.starts_with('/');
        let name = inside
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let style = match name.as_str() {
            "b" | "strong" => Some(Style::Strong),
            "i" | "em" => Some(Style::Emphasis),
            "del" | "s" | "strike" => Some(Style::Deleted),
            "u" | "ins" => Some(Style::Underline),
            "code" | "tt" => Some(Style::Code),
            "mark" => Some(Style::Mark),
            "sub" => Some(Style::Sub),
            "sup" => Some(Style::Sup),
            "tg-spoiler" => Some(Style::Spoiler),
            _ => None,
        };
        if closing {
            if let Some(index) = state.html.iter().rposition(|(tag, _, _)| tag == &name) {
                let (_, styles, link) = state.html[index].clone();
                state.html.truncate(index);
                state.styles = styles;
                state.link = link;
            }
        } else if let Some(style) = style {
            if state.html.len() < 24 {
                state
                    .html
                    .push((name, state.styles.clone(), state.link.clone()));
                state.styles.insert(style);
            }
        } else if name == "a" {
            if let Some(href) = attribute(inside, "href") {
                state
                    .html
                    .push((name, state.styles.clone(), state.link.clone()));
                state.link = Some(entities(&href));
            }
        } else if name == "br" {
            out.append("\n", &state.styles, state.link.as_deref());
        } else if matches!(
            name.as_str(),
            "p" | "div"
                | "section"
                | "article"
                | "main"
                | "header"
                | "footer"
                | "aside"
                | "nav"
                | "summary"
        ) {
            if !out.text.is_empty() && !out.text.ends_with('\n') {
                out.append("\n\n", &state.styles, None);
            }
        } else if !matches!(name.as_str(), "details" | "span" | "!--") {
            out.append(&source[start..end], &state.styles, state.link.as_deref());
        }
        at = end;
    }
}
fn attribute(tag: &str, key: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let start = lower.find(&format!("{key}="))? + key.len() + 1;
    let value = tag[start..].trim_start();
    let quote = value.chars().next()?;
    if quote == '\'' || quote == '"' {
        Some(value[1..value[1..].find(quote)? + 1].into())
    } else {
        Some(
            value
                .split_whitespace()
                .next()?
                .trim_end_matches('/')
                .into(),
        )
    }
}
fn entities(source: &str) -> String {
    let mut out = String::new();
    let mut tail = source;
    while let Some(start) = tail.find('&') {
        out.push_str(&tail[..start]);
        let Some(end) = tail[start..].find(';').filter(|n| *n <= 16) else {
            out.push_str(&tail[start..]);
            return out;
        };
        let end = start + end;
        let name = &tail[start + 1..end];
        let c = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => name
                .strip_prefix("#x")
                .and_then(|s| u32::from_str_radix(s, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|s| s.parse().ok()))
                .and_then(char::from_u32),
        };
        if let Some(c) = c {
            out.push(c);
        } else {
            out.push_str(&tail[start..=end]);
        }
        tail = &tail[end + 1..];
    }
    out.push_str(tail);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mobile_article_styles_tables_and_bounds() {
        let doc=parse("# Title\n\n<u>under</u> <mark>mark</mark> <sub>sub</sub> <sup>sup</sup> <tg-spoiler>secret</tg-spoiler>\n\n| Name | Count |\n| --- | --- |\n| Apples | 3 |\n\n- [x] done\n- [ ] next\n\n$x^2$\n\n$$\\frac{1}{2}$$").unwrap();
        assert!(doc.rich);
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b,Block::Table(rows) if rows.rows.len()==2))
        );
        let Block::Paragraph(text) = &doc.blocks[1] else {
            panic!()
        };
        for style in [
            Style::Underline,
            Style::Mark,
            Style::Sub,
            Style::Sup,
            Style::Spoiler,
        ] {
            assert!(text.ranges.iter().any(|r| r.has(style)));
        }
        assert!(parse(&"a".repeat(32769)).is_none());
        assert!(!parse("ordinary plain text").unwrap().rich);
    }
    #[test]
    fn literal_urls_and_strict_quotes() {
        let doc = parse("https://example.org/a_b_c\n\n>greentext\n> quote\nordinary continuation")
            .unwrap();
        let Block::Paragraph(text) = &doc.blocks[0] else {
            panic!()
        };
        assert_eq!(text.text, "https://example.org/a_b_c");
        assert!(text.ranges.is_empty());
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b,Block::Paragraph(t) if t.text==">greentext"))
        );
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b,Block::Paragraph(t) if t.text=="ordinary continuation"))
        );
        assert!(!safe_uri("javascript:alert(1)"));
        assert!(safe_uri("mailto:a@example.org"));
    }
}
impl RichText {
    pub fn slice(&self, start: usize, end: usize) -> Self {
        Self {
            text: self
                .text
                .chars()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect(),
            ranges: self
                .ranges
                .iter()
                .filter_map(|r| {
                    let (a, b) = (r.start.max(start), r.end.min(end));
                    (a < b).then(|| Range {
                        start: a - start,
                        end: b - start,
                        ..r.clone()
                    })
                })
                .collect(),
            links: self
                .links
                .iter()
                .filter_map(|l| {
                    let (a, b) = (l.start.max(start), l.end.min(end));
                    (a < b).then(|| Link {
                        start: a - start,
                        end: b - start,
                        uri: l.uri.clone(),
                    })
                })
                .collect(),
            math: self
                .math
                .iter()
                .filter(|m| m.start >= start && m.end <= end)
                .map(|m| Formula {
                    start: m.start - start,
                    end: m.end - start,
                    ..m.clone()
                })
                .collect(),
        }
    }
}

fn ordered_markers(source: &str) -> std::collections::VecDeque<String> {
    let mut markers = std::collections::VecDeque::new();
    let mut fence = None;
    for line in source.lines() {
        let trimmed = line.trim_start_matches(' ');
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let c = trimmed.chars().next().unwrap();
            if fence == Some(c) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(c);
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        let digits = line.bytes().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0
            && digits <= 9
            && line
                .as_bytes()
                .get(digits)
                .is_some_and(|b| matches!(b, b'.' | b')'))
            && line[digits + 1..].starts_with(char::is_whitespace)
        {
            markers.push_back(line[..digits].into());
        }
    }
    markers
}

fn details_region(source: &str) -> Option<(usize, usize, usize, usize, bool)> {
    let mut opened = None;
    let mut depth = 0;
    for (event, range) in Parser::new(source).into_offset_iter() {
        if let Event::Html(raw) | Event::InlineHtml(raw) = event {
            let mut at = 0;
            while let Some(start) = raw[at..].find('<') {
                let start = at + start;
                let end = start + raw[start..].find('>')? + 1;
                let tag = raw[start + 1..end - 1].trim();
                let closing = tag.starts_with('/');
                let name = tag
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('/');
                if name.eq_ignore_ascii_case("details") {
                    if closing {
                        if depth > 0 {
                            depth -= 1;
                            if depth == 0 {
                                let (a, b, open) = opened?;
                                return Some((a, b, range.start + start, range.start + end, open));
                            }
                        }
                    } else {
                        if depth == 0 {
                            opened = Some((
                                range.start + start,
                                range.start + end,
                                tag.split_whitespace()
                                    .skip(1)
                                    .any(|v| v == "open" || v.starts_with("open=")),
                            ));
                        }
                        depth += 1;
                        if depth > 24 {
                            return None;
                        }
                    }
                }
                at = end;
            }
        }
    }
    None
}
fn profile_blocks(source: &str, depth: usize, budget: &mut usize) -> Option<Vec<Block>> {
    if depth > 24 {
        return None;
    }
    if let Some((start, body_start, body_end, end, open)) = details_region(source) {
        let mut out = blocks(&ast(&prepare(&source[..start]))?, depth, budget)?;
        let inner = &source[body_start..body_end];
        let (title, body) = if let Some(summary) = inner.find("<summary>") {
            let close = inner[summary..].find("</summary>")? + summary;
            let title = rich(&ast(&inner[summary + 9..close])?, false);
            (
                title,
                format!("{}{}", &inner[..summary], &inner[close + 10..]),
            )
        } else {
            (RichText::plain("Details"), inner.to_string())
        };
        *budget = budget.checked_sub(1)?;
        out.push(Block::Details {
            title,
            blocks: profile_blocks(&body, depth + 1, budget)?,
            open,
        });
        out.extend(profile_blocks(&source[end..], depth, budget)?);
        Some(out)
    } else {
        blocks(&ast(&prepare(source))?, depth, budget)
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    #[test]
    fn details_across_paragraphs_and_source_numbers_follow_mobile() {
        let doc=parse("<details open>\n<summary>**Title**</summary>\n\nA <u>styled</u> paragraph.\n\n- one\n- two\n</details>\n\n3. three\n9. nine\n").unwrap();
        let Block::Details {
            title,
            blocks,
            open,
        } = &doc.blocks[0]
        else {
            panic!("{:?}", doc.blocks)
        };
        assert!(*open);
        assert_eq!(title.text, "Title");
        assert!(title.ranges.iter().any(|r| r.has(Style::Strong)));
        assert!(blocks.len() >= 2);
        let Block::List(Some(markers), _) = &doc.blocks[1] else {
            panic!()
        };
        assert_eq!(markers, &["3", "9"]);
    }
    #[test]
    fn chat_quote_lines_are_inline_and_preserve_breaks() {
        let doc = parse("> # literal heading\n> second line\nnormal\n").unwrap();
        let Block::Quote(blocks) = &doc.blocks[0] else {
            panic!()
        };
        let Block::Paragraph(text) = &blocks[0] else {
            panic!()
        };
        assert_eq!(text.text.trim_end(), "# literal heading\nsecond line");
        assert!(matches!(&doc.blocks[1],Block::Paragraph(t)if t.text=="normal"));
    }
}
