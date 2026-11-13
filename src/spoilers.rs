//! Preview-only spoiler metadata, matching mobile MessageMarkup (GPL-3.0-or-later).
//! Legacy hints can hide authenticated text; they never supply text or URLs.
use serde::{Deserialize, Serialize};
use tokio_xmpp::minidom::Element;
const CLIENT: &str = "jabber:client";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
const WHOLE: &str = "urn:xmpp:spoiler:0";
const MARKUP: &str = "urn:xmpp:markup:0";
const PARTIAL: &str = "urn:monogram:spoiler:0";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: usize,
    pub end: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub whole: bool,
    pub ranges: Vec<Range>,
    pub text_hash: Option<[u8; 32]>,
}
impl Context {
    pub fn clear() -> Self {
        Self {
            whole: false,
            ranges: Vec::new(),
            text_hash: None,
        }
    }
    pub fn matches(&self, text: &str) -> bool {
        let count = text.chars().count();
        self.ranges.len() <= 512
            && self
                .ranges
                .iter()
                .all(|range| range.start < range.end && range.end <= count)
            && self.text_hash.map_or(self.ranges.is_empty(), |hash| {
                hash == openssl::sha::sha256(text.as_bytes())
            })
    }
    pub fn intersects(&self, start: usize, end: usize) -> bool {
        self.whole
            || self
                .ranges
                .iter()
                .any(|range| range.start < end && range.end > start)
    }
    fn bind(mut self, text: &str) -> Self {
        self.text_hash = Some(openssl::sha::sha256(text.as_bytes()));
        self
    }
    fn remove(mut self, body: &str, intervals: &[(usize, usize)]) -> Option<(Self, String)> {
        let chars: Vec<_> = body.chars().collect();
        let mut removed = vec![false; chars.len()];
        for &(start, end) in intervals {
            if start > end || end > chars.len() {
                return None;
            }
            removed[start..end].fill(true);
        }
        let mut map = vec![0; chars.len() + 1];
        let mut text = String::new();
        for (index, character) in chars.into_iter().enumerate() {
            map[index + 1] = map[index];
            if !removed[index] {
                text.push(character);
                map[index + 1] += 1;
            }
        }
        self.ranges = self
            .ranges
            .into_iter()
            .filter_map(|range| {
                let (start, end) = (*map.get(range.start)?, *map.get(range.end)?);
                (start < end).then_some(Range { start, end })
            })
            .collect();
        Some((self.bind(&text), text))
    }
}
fn whole_hint(message: &Element) -> bool {
    let hints: Vec<_> = message
        .children()
        .filter(|child| child.is("spoiler", WHOLE))
        .collect();
    hints.len() <= 16
        && hints.into_iter().any(|hint| {
            hint.children().next().is_none()
                && hint.text().encode_utf16().count() <= 4096
                && hint
                    .attr_ns(XML, "lang")
                    .is_none_or(|language| Some(language) == message.attr_ns(XML, "lang"))
        })
}
fn partial_hint(message: &Element) -> bool {
    message
        .children()
        .filter(|child| child.is("markup", MARKUP))
        .flat_map(Element::children)
        .filter(|span| span.is("span", MARKUP))
        .flat_map(Element::children)
        .any(|child| child.is("spoiler", PARTIAL))
}
fn ranges(message: &Element, body: &str) -> Option<Vec<Range>> {
    let markups: Vec<_> = message
        .children()
        .filter(|child| child.is("markup", MARKUP))
        .collect();
    if markups.len() != 1 {
        return None;
    }
    let markup = markups[0];
    if markup
        .attr_ns(XML, "lang")
        .is_some_and(|language| Some(language) != message.attr_ns(XML, "lang"))
        || markup.children().count() > 256
    {
        return None;
    }
    let count = body.chars().count();
    let mut parsed: Vec<(Range, bool, bool)> = Vec::new();
    for element in markup.children().filter(|child| child.ns() == MARKUP) {
        let block = matches!(element.name(), "bcode" | "bquote");
        let hidden = element.is("span", MARKUP)
            && element
                .children()
                .any(|child| child.is("spoiler", PARTIAL) && child.children().next().is_none());
        let recognized = block
            || hidden
            || element.is("span", MARKUP)
                && element.children().any(|child| {
                    child.ns() == MARKUP
                        && matches!(child.name(), "emphasis" | "strong" | "code" | "deleted")
                });
        if !recognized {
            continue;
        }
        let index = |name| {
            let value = element.attr(name)?;
            if value.is_empty() || value.len() > 10 || !value.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            value.parse::<usize>().ok()
        };
        let (start, end) = (index("start")?, index("end")?);
        if start >= end
            || end > count
            || element.is("bcode", MARKUP)
                && element.attr("language").is_some_and(|language| {
                    language.encode_utf16().count() > 128 || language.chars().any(char::is_control)
                })
        {
            return None;
        }
        for (other, other_block, _) in &parsed {
            if start >= other.end || other.start >= end {
                continue;
            }
            if !block && !*other_block
                || block && !*other_block && (other.start < start || other.end > end)
                || *other_block && !block && (start < other.start || end > other.end)
                || block
                    && *other_block
                    && (start < other.start && end < other.end
                        || other.start < start && other.end < end)
            {
                return None;
            }
        }
        parsed.push((Range { start, end }, block, hidden));
    }
    Some(
        parsed
            .into_iter()
            .filter_map(|(range, _, hidden)| hidden.then_some(range))
            .collect(),
    )
}
fn metadata(message: &Element, body: &str) -> Context {
    Context {
        whole: whole_hint(message),
        ranges: ranges(message, body).unwrap_or_default(),
        text_hash: None,
    }
    .bind(body)
}
/// Project only the same validated fallback/prefix removals as the text normalizer.
/// A mismatch leaves the context unknown rather than guessing a repeated substring.
pub fn normalized(message: &Element, visible: &str) -> Option<Context> {
    let raw = message.get_child("body", CLIENT)?.text();
    let ((_, _, _, clean), removed) = crate::protocol::reply_projection(message, &raw);
    let (mut context, mut text) = metadata(message, &raw).remove(&raw, &removed)?;
    if text != clean {
        return None;
    }
    if let Some((attribution, body)) = crate::forwarding::parse(&text) {
        let prefix = attribution.prefix().chars().count();
        (context, text) = context.remove(&text, &[(0, prefix)])?;
        if text != body {
            return None;
        }
    } else if let Some((_, body)) = crate::forwarding::standalone(message) {
        let original = message
            .get_child("forwarded", crate::forwarding::NS)?
            .get_child("message", CLIENT)?;
        let inner = metadata(original, &body);
        let offset = if text.is_empty() {
            0
        } else {
            text.chars().count() + 2
        };
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&body);
        if inner.whole {
            if !body.is_empty() {
                context.ranges.push(Range {
                    start: offset,
                    end: offset + body.chars().count(),
                });
            }
        } else {
            context
                .ranges
                .extend(inner.ranges.into_iter().map(|range| Range {
                    start: range.start + offset,
                    end: range.end + offset,
                }));
        }
    }
    (text == visible).then(|| context.bind(visible))
}
/// Import only Legacy presentation hiding. No alternate body, file or style metadata is copied.
pub fn append_legacy_hints(outer: &Element, content: &mut Element) {
    let Some(body) = content.get_child("body", CLIENT).map(Element::text) else {
        return;
    };
    let context = metadata(outer, &body);
    if crate::formatting::from_element(outer).markdown && content.get_child("content",crate::formatting::CONTENT).is_none(){content.append_child(Element::builder("content",crate::formatting::CONTENT).attr("type".try_into().unwrap(),"text/markdown").build());}
    if context.whole {
        content.append_child(Element::builder("spoiler", WHOLE).build());
    }
    if !context.ranges.is_empty() {
        let mut markup = Element::builder("markup", MARKUP);
        for range in context.ranges {
            markup = markup.append(
                Element::builder("span", MARKUP)
                    .attr("start".try_into().unwrap(), range.start.to_string())
                    .attr("end".try_into().unwrap(), range.end.to_string())
                    .append(Element::builder("spoiler", PARTIAL).build())
                    .build(),
            );
        }
        content.append_child(markup.build());
    }
}
/// A known authenticated Legacy body may acquire public hiding hints from its MAM copy.
/// Old already-stripped replies cannot recover partial offsets after their quote was replaced.
pub fn cached_legacy(outer: &Element, stored: &crate::model::Message) -> Option<Context> {
    if whole_hint(outer) {
        return Some(
            Context {
                whole: true,
                ..Context::clear()
            }
            .bind(&stored.text),
        );
    }
    if !partial_hint(outer) {
        return Some(Context::clear().bind(&stored.text));
    }
    if !stored.reply_id.is_empty() {
        return None;
    }
    let raw = stored.forwarded.as_ref().map_or_else(
        || stored.text.clone(),
        |attribution| attribution.wire_body(&stored.text),
    );
    let mut source = Element::builder("message", CLIENT)
        .append(Element::builder("body", CLIENT).append(raw).build())
        .build();
    append_legacy_hints(outer, &mut source);
    normalized(&source, &stored.text)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_hiding_uses_codepoints_and_validated_reply_projection() {
        let raw = "> 😀 quote\nVisible https://example.org hidden";
        let prefix = "> 😀 quote\n".chars().count();
        let start = "> 😀 quote\nVisible ".chars().count();
        let end = start + "https://example.org".chars().count();
        let xml:Element=format!("<message xmlns='jabber:client' from='p@example.org' to='o@example.org'><body>{raw}</body><reply xmlns='urn:xmpp:reply:0' id='original'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'><body start='0' end='{prefix}'/></fallback><markup xmlns='urn:xmpp:markup:0'><span start='{start}' end='{end}'><spoiler xmlns='urn:monogram:spoiler:0'/></span></markup></message>").parse().unwrap();
        let visible = "Visible https://example.org hidden";
        let context = normalized(&xml, visible).unwrap();
        assert!(context.matches(visible));
        assert!(context.intersects(8, 27));
        assert!(!context.intersects(0, 7));
        assert!(!context.matches("https://another.example"));
        let mut message = crate::model::Message {
            text: visible.into(),
            preview_spoilers: Some(context.clone()),
            ..Default::default()
        };
        assert_eq!(
            crate::model::Message::decode(&serde_json::to_string(&message).unwrap())
                .unwrap()
                .preview_spoilers,
            Some(context)
        );
        let old = crate::model::Message::decode("{\"text\":\"https://example.org\"}").unwrap();
        assert!(old.preview_spoilers.is_none());
        assert!(crate::model::Message::default().preview_spoilers.is_some());
        message.preview_spoilers = Some(Context {
            whole: true,
            ..Context::clear()
        });
        assert!(message.preview_spoilers.unwrap().intersects(0, 1));
        let outer:Element="<message xmlns='jabber:client'><body>Never replace body</body><spoiler xmlns='urn:xmpp:spoiler:0'/><markup xmlns='urn:xmpp:markup:0'><span start='0' end='2'><spoiler xmlns='urn:monogram:spoiler:0'/><strong/></span></markup></message>".parse().unwrap();
        let mut content: Element =
            "<message xmlns='jabber:client'><body>Authenticated text</body></message>"
                .parse()
                .unwrap();
        append_legacy_hints(&outer, &mut content);
        assert_eq!(
            content.get_child("body", CLIENT).unwrap().text(),
            "Authenticated text"
        );
        assert!(metadata(&content, "Authenticated text").whole);
        assert!(
            content
                .children()
                .filter(|child| child.is("markup", MARKUP))
                .all(|markup| markup
                    .children()
                    .all(|span| span.children().all(|child| child.is("spoiler", PARTIAL))))
        );
    }
}
