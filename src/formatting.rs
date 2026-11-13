//! Mobile MessageMarkup's bounded code-point ranges and display projection.
//! Adapted from Monogram mobile (GPL-3.0-or-later); raw bodies remain unchanged.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use tokio_xmpp::minidom::Element;

pub const NS: &str = "urn:xmpp:markup:0";
pub const STYLING: &str = "urn:xmpp:styling:0";
pub const SPOILER: &str = "urn:xmpp:spoiler:0";
pub const PARTIAL: &str = "urn:monogram:spoiler:0";
pub const CONTENT: &str = "urn:xmpp:content";
const CLIENT: &str = "jabber:client";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
pub const MAX_RANGES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Style {
    Strong,
    Emphasis,
    Deleted,
    Code,
    Pre,
    Quote,
    Spoiler,
    Underline,
    Mark,
    Sub,
    Sup,
}
impl Style {
    pub fn block(self) -> bool {
        matches!(self, Self::Pre | Self::Quote)
    }
    fn wire(self) -> Option<&'static str> {
        Some(match self {
            Self::Strong => "strong",
            Self::Emphasis => "emphasis",
            Self::Deleted => "deleted",
            Self::Code => "code",
            Self::Spoiler => "spoiler",
            _ => return None,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: usize,
    pub end: usize,
    pub styles: BTreeSet<Style>,
    pub language: Option<String>,
}
impl Range {
    pub fn new(start: usize, end: usize, style: Style) -> Self {
        Self {
            start,
            end,
            styles: BTreeSet::from([style]),
            language: None,
        }
    }
    pub fn has(&self, style: Style) -> bool {
        self.styles.contains(&style)
    }
    fn block(&self) -> bool {
        self.styles.iter().any(|s| s.block())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mention {
    pub start: usize,
    pub end: usize,
    pub address: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Markup {
    pub body: String,
    pub ranges: Vec<Range>,
    #[serde(default)]
    pub mentions: Vec<Mention>,
    pub unstyled: bool,
    pub markdown: bool,
    pub spoiler: bool,
}
#[derive(Clone, Debug)]
pub struct Display {
    pub body: String,
    pub ranges: Vec<Range>,
    pub mentions: Vec<Mention>,
    pub removed: Vec<(usize, usize)>,
}
impl Markup {
    pub fn plain(body: &str) -> Self {
        Self {
            body: body.into(),
            ..Default::default()
        }
    }
    pub fn valid(&self) -> bool {
        let count = self.body.chars().count();
        self.mentions.len() <= 64
            && self.mentions.iter().enumerate().all(|(i, m)| {
                m.start < m.end
                    && m.end <= count
                    && m.address.len() <= 2048
                    && m.address
                        .parse::<tokio_xmpp::jid::BareJid>()
                        .is_ok_and(|jid| jid.node().is_some())
                    && self.mentions[..i]
                        .iter()
                        .all(|p| p.end <= m.start || m.end <= p.start)
            })
            && self.ranges.len() <= MAX_RANGES
            && self.ranges.iter().enumerate().all(|(i, a)| {
                a.start < a.end
                    && a.end <= count
                    && !a.styles.is_empty()
                    && (!a.block() || a.styles.len() == 1)
                    && a.language.as_ref().is_none_or(|v| {
                        v.encode_utf16().count() <= 128 && !v.chars().any(|c| c.is_control())
                    })
                    && self.ranges[..i].iter().all(|b| {
                        a.start >= b.end
                            || b.start >= a.end
                            || (a.block()
                                && b.block()
                                && (a.start <= b.start && a.end >= b.end
                                    || b.start <= a.start && b.end >= a.end))
                            || (a.block() && !b.block() && b.start >= a.start && b.end <= a.end)
                            || (!a.block() && b.block() && a.start >= b.start && a.end <= b.end)
                    })
            })
    }
    pub fn without(&self, intervals: &[(usize, usize)]) -> Option<Self> {
        let text: Vec<_> = self.body.chars().collect();
        let mut removed = vec![false; text.len()];
        for &(a, b) in intervals {
            if a > b || b > text.len() {
                return None;
            }
            removed[a..b].fill(true);
        }
        Some(self.project(&text, &removed, true))
    }
    fn project(&self, text: &[char], removed: &[bool], intact: bool) -> Self {
        let mut map = vec![0; text.len() + 1];
        let mut body = String::new();
        for (i, c) in text.iter().enumerate() {
            map[i + 1] = map[i];
            if !removed[i] {
                body.push(*c);
                map[i + 1] += 1;
            }
        }
        let ranges = self
            .ranges
            .iter()
            .filter_map(|r| {
                let (start, end) = (*map.get(r.start)?, *map.get(r.end)?);
                (start < end).then(|| Range {
                    start,
                    end,
                    ..r.clone()
                })
            })
            .collect();
        Self {
            body,
            ranges,
            mentions: self
                .mentions
                .iter()
                .filter_map(|m| {
                    let (a, b) = (*map.get(m.start)?, *map.get(m.end)?);
                    (a < b && (!intact || !removed[m.start..m.end].iter().any(|v| *v))).then(|| {
                        Mention {
                            start: a,
                            end: b,
                            address: m.address.clone(),
                        }
                    })
                })
                .collect(),
            ..self.clone()
        }
    }
    pub fn slice(&self, start: usize, end: usize) -> Option<Self> {
        let n = self.body.chars().count();
        if start > end || end > n {
            return None;
        }
        self.without(&[(0, start), (end, n)])
    }
    pub fn trim(&self) -> Self {
        let start = self.body.chars().take_while(|c| c.is_whitespace()).count();
        let end = self.body.chars().count()
            - self
                .body
                .chars()
                .rev()
                .take_while(|c| c.is_whitespace())
                .count();
        self.slice(start.min(end), end)
            .unwrap_or_else(|| Self::plain(""))
    }
    pub fn in_body(&self, body: &str, start: usize) -> Option<Self> {
        if body
            .chars()
            .skip(start)
            .take(self.body.chars().count())
            .collect::<String>()
            != self.body
        {
            return None;
        }
        Some(Self {
            body: body.into(),
            mentions: self
                .mentions
                .iter()
                .map(|m| Mention {
                    start: m.start + start,
                    end: m.end + start,
                    address: m.address.clone(),
                })
                .collect(),
            ranges: self
                .ranges
                .iter()
                .map(|r| Range {
                    start: r.start + start,
                    end: r.end + start,
                    ..r.clone()
                })
                .collect(),
            ..self.clone()
        })
    }
    pub fn display(&self) -> Display {
        if self.markdown || self.unstyled || self.body.chars().count() > 65536 || !self.valid() {
            return Display {
                body: self.body.clone(),
                ranges: self.ranges.clone(),
                mentions: self.mentions.clone(),
                removed: vec![],
            };
        }
        let mut parser = Styling {
            text: self.body.chars().collect(),
            removed: vec![false; self.body.chars().count()],
            ranges: vec![],
            declared: &self.ranges,
        };
        parser.parse();
        let mut source = self.clone();
        if source.ranges.is_empty() {
            source.ranges = normalize(&parser.ranges).unwrap_or_default();
        }
        let visible = source.project(&parser.text, &parser.removed, false);
        let mut removed = vec![];
        let mut i = 0;
        while i < parser.removed.len() {
            if !parser.removed[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < parser.removed.len() && parser.removed[i] {
                i += 1;
            }
            removed.push((start, i));
        }
        Display {
            body: visible.body,
            ranges: visible.ranges,
            mentions: visible.mentions,
            removed,
        }
    }
    pub fn spoiler_context(&self) -> crate::spoilers::Context {
        crate::spoilers::Context {
            whole: self.spoiler,
            ranges: self
                .ranges
                .iter()
                .filter(|r| r.has(Style::Spoiler))
                .map(|r| crate::spoilers::Range {
                    start: r.start,
                    end: r.end,
                })
                .collect(),
            text_hash: Some(openssl::sha::sha256(self.body.as_bytes())),
        }
    }
    pub fn add_to(&self, message: &mut Element) {
        if !self.valid()
            || message
                .get_child("body", CLIENT)
                .map(Element::text)
                .as_deref()
                != Some(&self.body)
        {
            return;
        }
        let mut markup = Element::builder("markup", NS);
        let mut count = 0;
        for r in &self.ranges {
            let tag = if r.has(Style::Pre) {
                "bcode"
            } else if r.has(Style::Quote) {
                "bquote"
            } else {
                "span"
            };
            let mut span = Element::builder(tag, NS)
                .attr("start".try_into().unwrap(), r.start.to_string())
                .attr("end".try_into().unwrap(), r.end.to_string());
            if tag == "bcode"
                && let Some(language) = r.language.as_ref().filter(|s| !s.is_empty())
            {
                span = span.attr("language".try_into().unwrap(), language);
            }
            let mut recognized = tag != "span";
            if tag == "span" {
                for style in &r.styles {
                    if let Some(name) = style.wire() {
                        span = span.append(
                            Element::builder(
                                name,
                                if *style == Style::Spoiler {
                                    PARTIAL
                                } else {
                                    NS
                                },
                            )
                            .build(),
                        );
                        recognized = true;
                    }
                }
            }
            if recognized {
                markup = markup.append(span.build());
                count += 1;
            }
        }
        if count > 0 {
            message.append_child(markup.build());
        }
        let mut end = 0;
        let mut hidden = self
            .ranges
            .iter()
            .filter(|r| r.has(Style::Spoiler))
            .collect::<Vec<_>>();
        hidden.sort_by_key(|r| r.start);
        for r in hidden {
            if r.start > end {
                break;
            }
            end = end.max(r.end);
        }
        if self.spoiler || !self.body.is_empty() && end == self.body.chars().count() {
            message.append_child(Element::builder("spoiler", SPOILER).build());
        }
        if self.markdown {
            message.append_child(
                Element::builder("content", CONTENT)
                    .attr("type".try_into().unwrap(), "text/markdown")
                    .build(),
            );
        }
        for m in &self.mentions {
            let uri = format!(
                "xmpp:{}",
                percent_encoding::utf8_percent_encode(
                    &m.address,
                    percent_encoding::NON_ALPHANUMERIC
                )
            );
            message.append_child(
                Element::builder("reference", "urn:xmpp:reference:0")
                    .attr("type".try_into().unwrap(), "mention")
                    .attr("uri".try_into().unwrap(), uri)
                    .attr("begin".try_into().unwrap(), m.start.to_string())
                    .attr("end".try_into().unwrap(), m.end.to_string())
                    .build(),
            );
        }
        if self.unstyled {
            message.append_child(Element::builder("unstyled", STYLING).build());
        }
    }
}

pub fn normalize(source: &[Range]) -> Option<Vec<Range>> {
    let points: BTreeSet<_> = source.iter().flat_map(|r| [r.start, r.end]).collect();
    let points: Vec<_> = points.into_iter().collect();
    let mut result: Vec<_> = source.iter().filter(|r| r.block()).cloned().collect();
    for pair in points.windows(2) {
        let styles = source
            .iter()
            .filter(|r| !r.block() && r.start <= pair[0] && r.end >= pair[1])
            .flat_map(|r| r.styles.iter().copied())
            .collect::<BTreeSet<_>>();
        if !styles.is_empty() {
            result.push(Range {
                start: pair[0],
                end: pair[1],
                styles,
                language: None,
            });
        }
        if result.len() > MAX_RANGES {
            return None;
        }
    }
    Some(result)
}

pub fn from_element(message: &Element) -> Markup {
    let body = message
        .get_child("body", CLIENT)
        .map(Element::text)
        .unwrap_or_default();
    let mut model = Markup::plain(&body);
    model.unstyled = message.get_child("unstyled", STYLING).is_some();
    let spoilers = message
        .children()
        .filter(|c| c.is("spoiler", SPOILER))
        .collect::<Vec<_>>();
    model.spoiler = spoilers.len() <= 16
        && spoilers.iter().any(|c| {
            c.children().next().is_none()
                && c.text().encode_utf16().count() <= 4096
                && c.attr_ns(XML, "lang")
                    .is_none_or(|v| Some(v) == message.attr_ns(XML, "lang"))
        });
    let hints = message
        .children()
        .filter(|c| c.is("content", CONTENT))
        .collect::<Vec<_>>();
    model.markdown = hints.len() == 1
        && hints[0].attr("type") == Some("text/markdown")
        && hints[0].attrs().into_iter().count() == 1
        && hints[0].children().next().is_none()
        && hints[0].text().is_empty();
    model.ranges = parse_ranges(message, &body).unwrap_or_default();
    model.mentions = parse_mentions(message, &body);
    model
}
fn index(element: &Element, key: &str) -> Option<usize> {
    let v = element.attr(key)?;
    (!v.is_empty() && v.len() <= 10 && v.bytes().all(|b| b.is_ascii_digit()))
        .then(|| v.parse().ok())
        .flatten()
}
fn parse_ranges(message: &Element, body: &str) -> Option<Vec<Range>> {
    let markup = message
        .children()
        .filter(|c| c.is("markup", NS))
        .collect::<Vec<_>>();
    if markup.len() != 1 {
        return None;
    }
    let markup = markup[0];
    if markup
        .attr_ns(XML, "lang")
        .is_some_and(|v| Some(v) != message.attr_ns(XML, "lang"))
        || markup.children().count() > MAX_RANGES
    {
        return None;
    }
    let mut ranges = vec![];
    for e in markup.children().filter(|c| c.ns() == NS) {
        let mut styles = BTreeSet::new();
        match e.name() {
            "bcode" => {
                styles.insert(Style::Pre);
            }
            "bquote" => {
                styles.insert(Style::Quote);
            }
            "span" => {
                for c in e.children() {
                    let style = match (c.name(), c.ns().as_str()) {
                        ("emphasis", NS) => Style::Emphasis,
                        ("strong", NS) => Style::Strong,
                        ("deleted", NS) => Style::Deleted,
                        ("code", NS) => Style::Code,
                        ("spoiler", PARTIAL) if c.children().next().is_none() => Style::Spoiler,
                        _ => continue,
                    };
                    styles.insert(style);
                }
            }
            _ => continue,
        }
        if styles.is_empty() {
            continue;
        }
        ranges.push(Range {
            start: index(e, "start")?,
            end: index(e, "end")?,
            styles,
            language: (e.name() == "bcode")
                .then(|| e.attr("language").map(str::to_owned))
                .flatten(),
        });
    }
    Markup {
        body: body.into(),
        ranges: ranges.clone(),
        ..Default::default()
    }
    .valid()
    .then_some(ranges)
}

pub fn normalized(message: &Element, visible: &str) -> Option<Markup> {
    let raw = message.get_child("body", CLIENT)?.text();
    let (_, mut removed) = crate::protocol::reply_projection(message, &raw);
    if let Some(file) = crate::file_share::Attachment::incoming(message, &raw, false)
        .or_else(|| crate::file_share::Attachment::incoming(message, &raw, true))
    {
        if model_body_is_uri_only(&raw, &removed, &file.wire_url) && visible.is_empty() {
            return Some(Markup {
                body: String::new(),
                ranges: vec![],
                ..from_element(message)
            });
        }
        for fallback in message.children().filter(|c| {
            c.is("fallback", "urn:xmpp:fallback:0")
                && c.attr("for")
                    .is_some_and(|v| matches!(v, "urn:xmpp:sfs:0" | "jabber:x:oob"))
        }) {
            for part in fallback
                .children()
                .filter(|c| c.is("body", "urn:xmpp:fallback:0") && c.attr_ns(XML, "lang").is_none())
            {
                let span = match (part.attr("start"), part.attr("end")) {
                    (None, None) => (0, raw.chars().count()),
                    (Some(_), Some(_)) => (index(part, "start")?, index(part, "end")?),
                    _ => return None,
                };
                if span.0 > span.1
                    || span.1 > raw.chars().count()
                    || raw
                        .chars()
                        .skip(span.0)
                        .take(span.1 - span.0)
                        .collect::<String>()
                        .trim_matches(|c| c <= '\u{20}')
                        != file.wire_url
                {
                    return None;
                }
                removed.push(span);
            }
        }
    }
    let mut model = from_element(message).without(&removed)?;
    if let Some((attribution, body)) = crate::forwarding::parse(&model.body) {
        model = model.slice(
            attribution.prefix().chars().count(),
            model.body.chars().count(),
        )?;
        if model.body != body {
            return None;
        }
    } else if let Some((_, body)) = crate::forwarding::standalone(message) {
        let inner = message
            .get_child("forwarded", crate::forwarding::NS)?
            .get_child("message", CLIENT)?;
        let nested = normalized(inner, &body).unwrap_or_else(|| Markup::plain(&body));
        let offset = model.body.chars().count() + if model.body.is_empty() { 0 } else { 2 };
        if !model.body.is_empty() {
            model.body.push_str("\n\n");
        }
        model.body.push_str(&body);
        let nested = nested.in_body(&model.body, offset)?;
        model.ranges.extend(nested.ranges);
        if nested.spoiler {
            model.ranges.push(Range::new(
                offset,
                model.body.chars().count(),
                Style::Spoiler,
            ));
        }
        model.markdown |= nested.markdown;
        model.mentions.extend(nested.mentions);
    }
    if model.body != visible && model.trim().body == visible {
        model = model.trim();
    }
    (model.body == visible && model.valid()).then_some(model)
}

/// Coordinates are rebased using generated prefixes/suffixes, never substring search.
pub fn decorate(message: &crate::model::Message, root: &mut Element) {
    if message.unavailable
        || message.poll.is_some()
        || matches!(
            message.action,
            Some(
                crate::message_actions::Action::React { .. }
                    | crate::message_actions::Action::Retract { .. }
            )
        )
    {
        return;
    }
    let Some(mut model) = message
        .formatting
        .clone()
        .filter(|m| m.body == message.text && m.valid())
    else {
        return;
    };
    if message.mode() == crate::encryption_policy::Mode::Legacy {
        model.ranges = model
            .ranges
            .iter()
            .filter(|r| r.has(Style::Spoiler))
            .map(|r| Range::new(r.start, r.end, Style::Spoiler))
            .collect();
        model.unstyled = false;
        model.mentions.clear();
    }
    let Some(raw) = root.get_child("body", CLIENT).map(Element::text) else {
        return;
    };
    let accompanied = message
        .forwarded
        .as_ref()
        .map(|a| a.wire_body(&message.text))
        .unwrap_or_else(|| message.text.clone());
    let suffix = message
        .attachment
        .as_ref()
        .map(|f| {
            if accompanied.is_empty() {
                f.wire_url.clone()
            } else {
                format!("\n{}", f.wire_url)
            }
        })
        .unwrap_or_default();
    let Some(without_suffix) = raw.strip_suffix(&suffix) else {
        return;
    };
    if !without_suffix.ends_with(&accompanied) {
        return;
    }
    let offset = without_suffix.chars().count() - message.text.chars().count();
    if let Some(model) = model.in_body(&raw, offset) {
        model.add_to(root);
    }
}

struct Styling<'a> {
    text: Vec<char>,
    removed: Vec<bool>,
    ranges: Vec<Range>,
    declared: &'a [Range],
}
impl Styling<'_> {
    fn parse(&mut self) {
        let infer_quotes = !self.declared.iter().any(|r| r.has(Style::Quote));
        let mut line = 0;
        while line < self.text.len() {
            let mut end = line;
            while end < self.text.len() && self.text[end] != '\n' {
                end += 1;
            }
            let mut start = line;
            let mut quote = 0;
            let quote_prefix = self
                .declared
                .iter()
                .filter(|r| r.has(Style::Quote))
                .any(|r| {
                    r.start >= 2
                        && self.text.get(r.start - 2..r.start) == Some(&['>', ' '])
                        && (r.start == 2 || self.text[r.start - 3] == '\n')
                        && (line == r.start - 2 || line > r.start && line < r.end)
                });
            while start + 1 < end
                && self.text[start] == '>'
                && self.text[start + 1] == ' '
                && quote < 8
                && (infer_quotes || quote == 0 && quote_prefix)
            {
                self.removed[start..start + 2].fill(true);
                start += 2;
                quote += 1;
            }
            if infer_quotes && quote > 0 && start < end {
                self.ranges.push(Range::new(start, end, Style::Quote));
            }
            if start + 2 < end && self.text[start..start + 3] == ['`', '`', '`'] {
                let content = (end + 1).min(self.text.len());
                let mut close = content;
                while close < self.text.len() {
                    let next = self.text[close..]
                        .iter()
                        .position(|c| *c == '\n')
                        .map_or(self.text.len(), |n| close + n);
                    if next - close == 3 && self.text[close..next] == ['`', '`', '`'] {
                        break;
                    }
                    close = (next + 1).min(self.text.len());
                }
                self.removed[start..content].fill(true);
                let mut stop = close;
                if close < self.text.len() {
                    if stop > content && self.text[stop - 1] == '\n' {
                        stop -= 1;
                        self.removed[stop] = true;
                    }
                    self.removed[close..(close + 3).min(self.text.len())].fill(true);
                }
                if stop > content {
                    self.ranges.push(Range::new(content, stop, Style::Pre));
                }
                line = (close + 3).min(self.text.len());
                if self.text.get(line) == Some(&'\n') {
                    line += 1;
                }
            } else {
                self.spans(start, end, 0);
                line = (end + 1).min(self.text.len());
            }
            if self.ranges.len() > MAX_RANGES {
                self.ranges.clear();
                self.removed.fill(false);
                return;
            }
        }
    }
    fn spans(&mut self, start: usize, end: usize, depth: usize) {
        if depth > 8 {
            return;
        }
        let mut i = start;
        while i < end {
            let marker = self.text[i];
            let style = match marker {
                '*' => Style::Strong,
                '_' => Style::Emphasis,
                '~' => Style::Deleted,
                '`' => Style::Code,
                _ => {
                    i += 1;
                    continue;
                }
            };
            if i + 1 >= end
                || self.text[i + 1].is_whitespace()
                || self.text[i + 1] == marker
                || i != start && !self.text[i - 1].is_whitespace()
            {
                i += 1;
                continue;
            }
            let mut close = i + 2;
            while close < end
                && (self.text[close] != marker || self.text[close - 1].is_whitespace())
            {
                close += 1;
            }
            if close >= end {
                i += 1;
                continue;
            }
            self.removed[i] = true;
            self.removed[close] = true;
            self.ranges.push(Range::new(i + 1, close, style));
            if style != Style::Code {
                self.spans(i + 1, close, depth + 1);
            }
            i = close + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mobile_styling_vectors_preserve_body_and_project_nested_unicode() {
        let value = Markup::plain("😀 *strong _nested_* ~gone~ `*literal*`");
        let display = value.display();
        assert_eq!(display.body, "😀 strong nested gone *literal*");
        assert_eq!(value.body, "😀 *strong _nested_* ~gone~ `*literal*`");
        assert!(
            display
                .ranges
                .iter()
                .any(|r| r.has(Style::Strong) && r.has(Style::Emphasis))
        );
        for literal in [
            "https://example.org/a_b_c",
            "word*not strong*",
            "**",
            "***",
            "* bad*",
            "*bad *",
            "*cross\nline*",
            ">greentext",
        ] {
            assert_eq!(Markup::plain(literal).display().body, literal);
        }
        assert_eq!(
            Markup::plain("> quote\n>greentext\n> > nested quote")
                .display()
                .body,
            "quote\n>greentext\nnested quote"
        );
        assert_eq!(
            Markup::plain("```\na *literal*\n```").display().body,
            "a *literal*"
        );
    }
    #[test]
    fn markup_roundtrip_and_malformed_ranges_follow_mobile() {
        let source:Element="<message xmlns='jabber:client'><body>😀 secret text</body><markup xmlns='urn:xmpp:markup:0'><span start='2' end='8'><strong/><spoiler xmlns='urn:monogram:spoiler:0'/></span></markup><unstyled xmlns='urn:xmpp:styling:0'/></message>".parse().unwrap();
        let model = from_element(&source);
        assert!(model.valid());
        assert!(model.ranges[0].has(Style::Spoiler));
        assert!(model.unstyled);
        let mut copy: Element =
            "<message xmlns='jabber:client'><body>😀 secret text</body></message>"
                .parse()
                .unwrap();
        model.add_to(&mut copy);
        assert_eq!(from_element(&copy), model);
        let bad:Element="<message xmlns='jabber:client'><body>test</body><markup xmlns='urn:xmpp:markup:0'><span start='0' end='99'><strong/></span></markup></message>".parse().unwrap();
        assert!(from_element(&bad).ranges.is_empty());
        let mut literal = Markup::plain("keep *literal* text");
        literal.unstyled = true;
        assert_eq!(literal.display().body, literal.body);
        let json = serde_json::to_string(&model).unwrap();
        assert_eq!(serde_json::from_str::<Markup>(&json).unwrap(), model);
    }
    #[test]
    fn exact_reply_projection_rebases_spoiler_ranges() {
        let xml:Element="<message xmlns='jabber:client'><body>&gt; 😀 quote\n😀 secret text</body><reply xmlns='urn:xmpp:reply:0' id='old'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'><body start='0' end='10'/></fallback><markup xmlns='urn:xmpp:markup:0'><span start='12' end='18'><spoiler xmlns='urn:monogram:spoiler:0'/></span></markup></message>".parse().unwrap();
        let model = normalized(&xml, "😀 secret text").unwrap();
        assert_eq!((model.ranges[0].start, model.ranges[0].end), (2, 8));
        assert!(normalized(&xml, "secret text").is_none());
    }
}

impl Markup {
    /// Use the same readable directive fallback and exact round-trip guard as mobile.
    pub fn styled(&self) -> Self {
        if self.markdown {
            return self.clone();
        }
        if self.ranges.is_empty() {
            return Self {
                unstyled: true,
                ..self.clone()
            };
        }
        if self.ranges.iter().any(|r| {
            r.styles
                .iter()
                .any(|s| matches!(s, Style::Underline | Style::Mark | Style::Sub | Style::Sup))
        }) {
            return self.rich_markdown();
        }
        let preserving = || Self {
            unstyled: true,
            ..self.clone()
        };
        if self.ranges.iter().any(|r| r.has(Style::Spoiler)) {
            return preserving();
        }
        let text: Vec<_> = self.body.chars().collect();
        let mut opens = vec![String::new(); text.len() + 1];
        let mut closes = opens.clone();
        let mut directives = self
            .ranges
            .iter()
            .filter(|r| r.block())
            .cloned()
            .collect::<Vec<_>>();
        for style in [Style::Strong, Style::Emphasis, Style::Deleted, Style::Code] {
            let mut parts = self
                .ranges
                .iter()
                .filter(|r| r.has(style))
                .map(|r| (r.start, r.end))
                .collect::<Vec<_>>();
            parts.sort();
            let mut joined: Vec<(usize, usize)> = vec![];
            for (a, b) in parts {
                if let Some(last) = joined.last_mut().filter(|last| a <= last.1) {
                    last.1 = last.1.max(b);
                } else {
                    joined.push((a, b));
                }
            }
            directives.extend(joined.into_iter().map(|(a, b)| Range::new(a, b, style)));
        }
        directives.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
        for r in directives {
            let (open, close) = if r.has(Style::Pre) {
                if r.start != 0 && text[r.start - 1] != '\n'
                    || r.end < text.len() && text[r.end] != '\n'
                {
                    return preserving();
                }
                ("```\n", "\n```")
            } else if r.has(Style::Quote) {
                if r.start != 0 && text[r.start - 1] != '\n'
                    || r.end < text.len() && text[r.end] != '\n'
                {
                    return preserving();
                }
                opens[r.start].push_str("> ");
                for i in r.start..r.end.saturating_sub(1) {
                    if text[i] == '\n' {
                        opens[i + 1].push_str("> ");
                    }
                }
                continue;
            } else if r.has(Style::Strong) {
                ("*", "*")
            } else if r.has(Style::Emphasis) {
                ("_", "_")
            } else if r.has(Style::Deleted) {
                ("~", "~")
            } else {
                ("`", "`")
            };
            opens[r.start].push_str(open);
            closes[r.end].insert_str(0, close);
        }
        let mut body = String::new();
        let mut starts = vec![0; text.len() + 1];
        let mut ends = starts.clone();
        let mut at = 0;
        for i in 0..=text.len() {
            body.push_str(&closes[i]);
            at += closes[i].chars().count();
            ends[i] = at;
            body.push_str(&opens[i]);
            at += opens[i].chars().count();
            starts[i] = at;
            if let Some(c) = text.get(i) {
                body.push(*c);
                at += 1;
            }
        }
        let encoded = Self {
            body,
            mentions: self
                .mentions
                .iter()
                .map(|m| Mention {
                    start: starts[m.start],
                    end: ends[m.end],
                    address: m.address.clone(),
                })
                .collect(),
            ranges: self
                .ranges
                .iter()
                .map(|r| Range {
                    start: starts[r.start],
                    end: ends[r.end],
                    ..r.clone()
                })
                .collect(),
            unstyled: false,
            ..self.clone()
        };
        if encoded.valid() && encoded.display().body == self.body {
            encoded
        } else {
            preserving()
        }
    }
    pub fn rich_markdown(&self) -> Self {
        let text: Vec<_> = self.body.chars().collect();
        let mut source = String::new();
        let mut points: BTreeSet<_> = self.ranges.iter().flat_map(|r| [r.start, r.end]).collect();
        points.extend([0, text.len()]);
        let points: Vec<_> = points.into_iter().collect();
        for pair in points.windows(2) {
            let styles = self
                .ranges
                .iter()
                .filter(|r| r.start <= pair[0] && r.end >= pair[1])
                .flat_map(|r| r.styles.iter().copied())
                .collect::<BTreeSet<_>>();
            let quoted = styles.contains(&Style::Quote);
            let pre = styles.contains(&Style::Pre);
            if (quoted || pre)
                && !source.is_empty()
                && !source.ends_with('\n')
                && pair[0] > 0
                && !self.ranges.iter().any(|r| {
                    r.start < pair[0]
                        && r.end > pair[0]
                        && (r.has(Style::Quote) || r.has(Style::Pre))
                })
            {
                source.push('\n');
            }
            if quoted && (source.is_empty() || source.ends_with('\n')) {
                source.push_str("> ");
            }
            if pre {
                source.push_str("<code>");
            }
            let mut closes = vec![];
            for style in styles {
                if matches!(style, Style::Quote | Style::Pre) {
                    continue;
                }
                let tag = match style {
                    Style::Strong => "b",
                    Style::Emphasis => "i",
                    Style::Deleted => "del",
                    Style::Code => "code",
                    Style::Pre => unreachable!(),
                    Style::Spoiler => "tg-spoiler",
                    Style::Underline => "u",
                    Style::Mark => "mark",
                    Style::Sub => "sub",
                    Style::Sup => "sup",
                    Style::Quote => unreachable!(),
                };
                source.push_str(&format!("<{tag}>"));
                closes.push(tag);
            }
            for c in &text[pair[0]..pair[1]] {
                match c {
                    '&' => source.push_str("&amp;"),
                    '<' => source.push_str("&lt;"),
                    '>' => source.push_str("&gt;"),
                    '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+'
                    | '.' | '!' | '|' | '~' | '$' | '-' => {
                        source.push('\\');
                        source.push(*c);
                    }
                    '\n' => {
                        source.push_str("<br>\n");
                        if quoted {
                            source.push_str("> ");
                        }
                    }
                    _ => source.push(*c),
                }
            }
            for tag in closes.into_iter().rev() {
                source.push_str(&format!("</{tag}>"));
            }
            if pre {
                source.push_str("</code>");
            }
            if (quoted || pre)
                && !self.ranges.iter().any(|r| {
                    r.start <= pair[0]
                        && r.end > pair[1]
                        && (r.has(Style::Quote) || r.has(Style::Pre))
                })
            {
                source.push_str("\n\n");
            }
        }
        Self {
            body: source,
            markdown: true,
            unstyled: true,
            spoiler: self.spoiler,
            ..Default::default()
        }
    }
}

pub fn remap_inline(
    model: &Markup,
    inline: &crate::packs::InlineEmoji,
) -> Option<crate::packs::InlineEmoji> {
    if model.body == inline.body {
        return Some(inline.clone());
    }
    if model.markdown {
        return None;
    }
    let display = model.display();
    if display.body != inline.body {
        return None;
    }
    let mut kept = vec![];
    for (i, _) in model.body.chars().enumerate() {
        if !display.removed.iter().any(|&(a, b)| a <= i && i < b) {
            kept.push(i);
        }
    }
    let ranges = inline
        .ranges
        .iter()
        .map(|r| {
            Some(crate::packs::InlineRange {
                start: *kept.get(r.start)?,
                end: kept.get(r.end - 1)? + 1,
                ..r.clone()
            })
        })
        .collect::<Option<Vec<_>>>()?;
    crate::packs::InlineEmoji::new(model.body.clone(), ranges).ok()
}

fn model_body_is_uri_only(raw: &str, removed: &[(usize, usize)], uri: &str) -> bool {
    Markup::plain(raw)
        .without(removed)
        .is_some_and(|m| m.body == uri)
}
pub fn validate_protected(message: &Element) -> bool {
    let markup = message
        .children()
        .filter(|c| c.is("markup", NS))
        .collect::<Vec<_>>();
    let hints = message
        .children()
        .filter(|c| c.is("unstyled", STYLING))
        .collect::<Vec<_>>();
    if markup.len() > 1 || hints.len() > 1 {
        return false;
    }
    if let Some(markup) = markup.first() {
        let mut bytes = vec![];
        if markup.write_to(&mut bytes).is_err() || bytes.len() > 65536 {
            return false;
        }
        let mut nodes = vec![(*markup, 0)];
        let mut count = 0;
        while let Some((node, depth)) = nodes.pop() {
            count += 1;
            if depth > 16 || count > 2048 {
                return false;
            }
            nodes.extend(node.children().map(|c| (c, depth + 1)));
        }
        let Some(body) = message.get_child("body", CLIENT) else {
            return false;
        };
        if parse_ranges(message, &body.text()).is_none() {
            return false;
        }
    }
    hints.first().is_none_or(|hint| {
        hint.attrs().into_iter().next().is_none()
            && hint.children().next().is_none()
            && hint.text().is_empty()
    })
}
pub fn preview(message: &crate::model::Message) -> String {
    let model = message
        .formatting
        .clone()
        .filter(|m| m.body == message.text && m.valid())
        .unwrap_or_else(|| Markup::plain(&message.text));
    if model.spoiler
        || message
            .preview_spoilers
            .as_ref()
            .is_some_and(|s| s.matches(&message.text) && s.whole)
    {
        return "Spoiler".into();
    }
    let mut display = model.display();
    if display.ranges.is_empty()
        && let Some(hints) = message
            .preview_spoilers
            .as_ref()
            .filter(|s| s.matches(&message.text))
    {
        let mapped = Markup {
            body: message.text.clone(),
            ranges: hints
                .ranges
                .iter()
                .map(|r| Range::new(r.start, r.end, Style::Spoiler))
                .collect(),
            ..Default::default()
        }
        .display();
        display.ranges = mapped.ranges;
    }
    let mut out = String::new();
    let mut hidden = false;
    for (i, c) in display.body.chars().enumerate() {
        if display
            .ranges
            .iter()
            .any(|r| r.has(Style::Spoiler) && r.start <= i && i < r.end)
        {
            if !hidden {
                out.push_str("[Spoiler]");
            }
            hidden = true;
        } else {
            hidden = false;
            out.push(c);
        }
    }
    if out.to_ascii_lowercase().contains("<tg-spoiler") {
        return "Spoiler".into();
    }
    out
}
fn parse_mentions(message: &Element, body: &str) -> Vec<Mention> {
    let references = message
        .children()
        .filter(|c| c.is("reference", "urn:xmpp:reference:0"))
        .collect::<Vec<_>>();
    if references.len() > 64 {
        return vec![];
    }
    let parsed = (|| -> Option<Vec<Mention>> {
        let mut result = vec![];
        for reference in references {
            if reference.attr("type") != Some("mention")
                || reference.attr("anchor").is_some()
                || reference.children().next().is_some()
            {
                continue;
            }
            let Some(uri) = reference.attr("uri").filter(|u| u.len() <= 4096) else {
                continue;
            };
            let Some(raw) = uri.strip_prefix("xmpp:") else {
                continue;
            };
            if raw.contains(['?', '#']) || raw.starts_with("//") {
                continue;
            }
            let address = percent_encoding::percent_decode_str(raw)
                .decode_utf8()
                .ok()?
                .into_owned();
            let jid = address.parse::<tokio_xmpp::jid::BareJid>().ok()?;
            if jid.node().is_none() {
                return None;
            }
            result.push(Mention {
                start: index(reference, "begin")?,
                end: index(reference, "end")?,
                address: jid.to_string(),
            });
        }
        result.sort_by_key(|m| m.start);
        let model = Markup {
            body: body.into(),
            mentions: result.clone(),
            ..Default::default()
        };
        model.valid().then_some(result)
    })();
    parsed.unwrap_or_default()
}

#[cfg(test)]
mod mobile_vectors {
    use super::*;
    #[test]
    fn actual_mobile_message_markup_vectors() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../tests/mobile-reference/formatting-vectors.json"
        ))
        .unwrap();
        for case in cases {
            let source = case["source"].as_str().unwrap();
            let xml: Element = case["xml"].as_str().unwrap().parse().unwrap();
            let parsed = from_element(&xml);
            assert_eq!(parsed.unstyled, case["unstyled"].as_bool().unwrap());
            assert_eq!(parsed.markdown, case["markdown"].as_bool().unwrap());
            if case["typed"].as_bool().unwrap() {
                assert_eq!(
                    Markup::plain(source).styled(),
                    parsed,
                    "mobile native authoring {source:?}"
                );
            }
            let display = parsed.display();
            assert_eq!(
                display.body,
                case["display"].as_str().unwrap(),
                "mobile source {source:?}"
            );
            let expected: Vec<Range> = serde_json::from_value(case["ranges"].clone()).unwrap();
            assert_eq!(display.ranges, expected, "mobile ranges {source:?}");
        }
    }
    #[test]
    fn authenticated_formatting_survives_sce_and_reply_caption_projection() {
        let clear:Element="<message xmlns='jabber:client' from='a@example.org/a' to='b@example.org' type='chat'><body>😀 secret</body><markup xmlns='urn:xmpp:markup:0'><span start='2' end='8'><spoiler xmlns='urn:monogram:spoiler:0'/><strong/></span></markup><unstyled xmlns='urn:xmpp:styling:0'/></message>".parse().unwrap();
        let plain = crate::crypto::payload::envelope(&clear).unwrap();
        let outer:Element="<message xmlns='jabber:client' from='a@example.org/a' to='b@example.org' type='chat'><body>public fallback</body><markup xmlns='urn:xmpp:markup:0'><span start='0' end='1'><deleted/></span></markup></message>".parse().unwrap();
        let content = crate::crypto::payload::content(&outer, &plain, 0).unwrap();
        assert_eq!(from_element(&content), from_element(&clear));
        let mut invalid = clear.clone();
        invalid.append_child(Element::builder("markup", NS).build());
        assert!(!validate_protected(&invalid));
        let text = "😀 caption";
        let markup = Markup {
            body: text.into(),
            ranges: vec![Range::new(2, 9, Style::Strong)],
            unstyled: true,
            ..Default::default()
        };
        let mut message = crate::model::Message {
            text: text.into(),
            formatting: Some(markup.clone()),
            wire_id: "file".into(),
            reply_id: "old".into(),
            reply_to: "a@example.org".into(),
            attachment: Some(
                crate::file_share::Attachment::bare("https://example.org/file.png").unwrap(),
            ),
            ..Default::default()
        };
        message.set_encryption(crate::encryption_policy::Mode::Plain, String::new());
        let wire = crate::protocol::outgoing("b@example.org", &message, "😀 quote");
        let received = normalized(&wire, text).unwrap();
        assert_eq!(received.ranges, markup.ranges);
    }
}

#[cfg(test)]
mod article_roundtrip_tests {
    use super::*;
    #[test]
    fn extra_styles_and_quote_use_mobile_markdown() {
        let model = Markup {
            body: "quoted styled\nsecond line".into(),
            ranges: vec![
                Range::new(0, 25, Style::Quote),
                Range::new(7, 13, Style::Underline),
            ],
            ..Default::default()
        };
        let wire = model.styled();
        assert!(wire.markdown);
        assert!(!wire.body.contains("blockquote"));
        let doc = crate::markdown::parse(&wire.body).unwrap();
        let crate::markdown::Block::Quote(blocks) = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks)
        };
        let crate::markdown::Block::Paragraph(text) = &blocks[0] else {
            panic!()
        };
        assert!(text.text.starts_with("quoted styled"));
        assert!(text.ranges.iter().any(|r| r.has(Style::Underline)));
    }
}
