//! Structured Reaction Polls ProtoXEP 0.0.2; the optional body carries no poll state.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use tokio_xmpp::minidom::Element;
pub const NS: &str = "urn:xmpp:polls:0";
pub const MAX_BYTES: usize = 65536;
const FALLBACK: &str = "urn:xmpp:fallback:0";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionValue {
    pub emoji: String,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Poll {
    pub question: String,
    pub description: String,
    pub options: Vec<OptionValue>,
    pub multiple: bool,
    pub closed: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub counts: Vec<u32>,
    pub chosen: BTreeSet<usize>,
    pub ballots: std::collections::BTreeMap<String, BTreeSet<usize>>,
}
impl Poll {
    pub fn readable_body(&self) -> String {
        let mut body = self.question.clone();
        if !self.description.is_empty() {
            body.push_str("\n\n");
            body.push_str(&self.description);
        }
        for option in &self.options {
            body.push_str(&format!("\n{} {}", option.emoji, option.label));
        }
        body.push_str(if self.closed {
            "\n\nVoting closed."
        } else if self.multiple {
            "\n\nPublic votes. Choose any options."
        } else {
            "\n\nPublic votes. Choose one option."
        });
        body
    }
    pub fn is_option(&self, emoji: &str) -> bool {
        self.options
            .iter()
            .any(|o| o.emoji.replace('\u{fe0f}', "") == emoji.replace('\u{fe0f}', ""))
    }
    pub fn tally(
        &self,
        sets: &std::collections::BTreeMap<bool, BTreeSet<String>>,
        owner: &str,
        peer: &str,
    ) -> Tally {
        let mut tally = Tally {
            counts: vec![0; self.options.len()],
            ..Default::default()
        };
        for (own, set) in sets {
            let chosen: BTreeSet<_> = self
                .options
                .iter()
                .enumerate()
                .filter(|(_, o)| {
                    set.iter()
                        .any(|e| e.replace('\u{fe0f}', "") == o.emoji.replace('\u{fe0f}', ""))
                })
                .map(|(i, _)| i)
                .collect();
            if chosen.is_empty() || !self.multiple && chosen.len() != 1 {
                continue;
            }
            for i in &chosen {
                tally.counts[*i] += 1;
            }
            if *own {
                tally.chosen = chosen.clone();
            }
            tally
                .ballots
                .insert(if *own { owner } else { peer }.into(), chosen);
        }
        tally
    }
}
fn field(value: &str, max: usize) -> bool {
    value.chars().count()<=max && value.chars().any(|c|!c.is_whitespace()) && value.chars().all(|c|matches!(c,'\t'|'\n'|'\r'|'\u{20}'..='\u{d7ff}'|'\u{e000}'..='\u{fffd}'|'\u{10000}'..='\u{10ffff}'))
}
impl Poll {
    pub fn validate(&self) -> Result<(), ()> {
        if !field(&self.question, 4096)
            || (!self.description.is_empty() && !field(&self.description, 4096))
            || !(2..=32).contains(&self.options.len())
        {
            return Err(());
        }
        let mut keys = BTreeSet::new();
        for option in &self.options {
            if option.emoji.encode_utf16().count() > 128 {
                return Err(());
            }
            let Some(emoji) = crate::message_actions::canonical_emoji(&option.emoji) else {
                return Err(());
            };
            if !keys.insert(emoji.replace('\u{fe0f}', "")) || !field(&option.label, 1024) {
                return Err(());
            }
        }
        let mut bytes = Vec::new();
        self.element().write_to(&mut bytes).map_err(|_| ())?;
        if bytes.len() > MAX_BYTES {
            return Err(());
        }
        Ok(())
    }
    /// Store the same canonical text/emoji values that the XML serializer puts on the wire.
    pub fn canonicalized(&self) -> Result<Self, ()> {
        self.validate()?;
        let mut bytes = Vec::new();
        self.element().write_to(&mut bytes).map_err(|_| ())?;
        let element: Element = String::from_utf8(bytes)
            .map_err(|_| ())?
            .parse()
            .map_err(|_| ())?;
        let message = Element::builder("message", "jabber:client")
            .append(element)
            .build();
        Self::parse(&message)?.ok_or(())
    }
    pub fn element(&self) -> Element {
        let mut root = Element::builder("poll", NS)
            .attr(
                "type".try_into().unwrap(),
                if self.multiple { "multiple" } else { "single" },
            )
            .build();
        if self.closed {
            root.set_attr("".into(), "closed".try_into().unwrap(), "true");
        }
        root.append_child(
            Element::builder("question", NS)
                .append(self.question.as_str())
                .build(),
        );
        if !self.description.is_empty() {
            root.append_child(
                Element::builder("description", NS)
                    .append(self.description.as_str())
                    .build(),
            );
        }
        for option in &self.options {
            root.append_child(
                Element::builder("option", NS)
                    .attr(
                        "emoji".try_into().unwrap(),
                        crate::message_actions::canonical_emoji(&option.emoji)
                            .unwrap_or_else(|| option.emoji.clone()),
                    )
                    .append(option.label.as_str())
                    .build(),
            );
        }
        root
    }
    pub fn parse(message: &Element) -> Result<Option<Self>, ()> {
        let mut roots = message.children().filter(|c| c.is("poll", NS));
        let Some(root) = roots.next() else {
            return Ok(None);
        };
        if roots.next().is_some() || root.text().chars().any(|c| !c.is_whitespace()) {
            return Err(());
        }
        let mut bytes = Vec::new();
        root.write_to(&mut bytes).map_err(|_| ())?;
        if bytes.len() > MAX_BYTES {
            return Err(());
        }
        let multiple = match root.attr("type") {
            Some("single") => false,
            Some("multiple") => true,
            _ => return Err(()),
        };
        let closed = match root.attr("closed") {
            None | Some("false" | "0") => false,
            Some("true" | "1") => true,
            _ => return Err(()),
        };
        let mut question = None;
        let mut description = None;
        let mut options = Vec::new();
        for child in root.children() {
            if child.ns() != NS || child.children().next().is_some() {
                return Err(());
            }
            match child.name() {
                "question" if question.is_none() => {
                    let value = child.text();
                    if !field(&value, 4096) {
                        return Err(());
                    }
                    question = Some(value);
                }
                "description" if description.is_none() => {
                    let value = child.text();
                    if !field(&value, 4096) {
                        return Err(());
                    }
                    description = Some(value);
                }
                "option" if options.len() < 32 => {
                    let raw = child.attr("emoji").ok_or(())?;
                    if raw.encode_utf16().count() > 128 {
                        return Err(());
                    }
                    let emoji = crate::message_actions::canonical_emoji(raw).ok_or(())?;
                    options.push(OptionValue {
                        emoji,
                        label: child.text(),
                    });
                }
                _ => return Err(()),
            }
        }
        let poll = Self {
            question: question.ok_or(())?,
            description: description.unwrap_or_default(),
            options,
            multiple,
            closed,
        };
        poll.validate()?;
        Ok(Some(poll))
    }
    pub fn validate_protected(message: &Element) -> Result<(), ()> {
        if Self::parse(message)?.is_some()
            && message.children().any(|c| {
                matches!(
                    (c.name(), c.ns().as_str()),
                    ("reactions", "urn:xmpp:reactions:0")
                        | ("retract", "urn:xmpp:message-retract:1")
                        | ("apply-to", "urn:xmpp:fasten:0")
                        | ("file-sharing", "urn:xmpp:sfs:0")
                )
            })
        {
            return Err(());
        }
        Ok(())
    }
    pub fn same_definition(&self, other: &Self) -> bool {
        self.question == other.question
            && self.description == other.description
            && self.multiple == other.multiple
            && self.options.len() == other.options.len()
            && self.options.iter().zip(&other.options).all(|(a, b)| {
                a.label == b.label
                    && a.emoji.replace('\u{fe0f}', "") == b.emoji.replace('\u{fe0f}', "")
            })
    }
    pub fn accompanying_body(&self, message: &Element) -> String {
        Self::body_without_fallbacks(message, &[NS])
    }
    pub fn body_without_fallbacks(message: &Element, namespaces: &[&str]) -> String {
        let body = message
            .get_child("body", "jabber:client")
            .map(|b| b.text())
            .unwrap_or_default();
        let chars: Vec<_> = body.chars().collect();
        let length = chars.len();
        let mut ranges = Vec::new();
        for fallback in message.children().filter(|c| {
            c.is("fallback", FALLBACK) && c.attr("for").is_some_and(|ns| namespaces.contains(&ns))
        }) {
            if fallback.text().chars().any(|c| !c.is_whitespace()) {
                return body;
            }
            if fallback.children().next().is_none() {
                if ranges.len() >= 64 {
                    return body;
                }
                ranges.push((0, length));
            }
            for text in fallback.children().filter(|c| {
                c.is("body", FALLBACK)
                    && c.attr_ns("http://www.w3.org/XML/1998/namespace", "lang")
                        .is_none()
            }) {
                if text.children().next().is_some() || !text.text().is_empty() || ranges.len() >= 64
                {
                    return body;
                }
                let (start, end) = match (text.attr("start"), text.attr("end")) {
                    (None, None) => (0, length),
                    (Some(a), Some(b)) => match (a.parse::<usize>(), b.parse::<usize>()) {
                        (Ok(a), Ok(b)) => (a, b),
                        _ => return body,
                    },
                    _ => return body,
                };
                if start > end || end > length {
                    return body;
                }
                ranges.push((start, end));
            }
        }
        ranges.sort();
        let mut pos = 0;
        let mut clean = String::new();
        for (a, b) in ranges {
            if a > pos {
                clean.extend(&chars[pos..a]);
            }
            pos = pos.max(b);
        }
        clean.extend(&chars[pos..]);
        clean
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn message(extra: &str) -> Element {
        format!("<message xmlns='jabber:client'>{extra}</message>")
            .parse()
            .unwrap()
    }
    const POLL: &str = "<poll xmlns='urn:xmpp:polls:0' type='single'><question>Where 😀?</question><option emoji='🍕'>Pizza</option><option emoji='🍜'>Noodles</option></poll>";
    #[test]
    fn explicit_fields_ignore_optional_fallback_and_accept_bodyless() {
        let plain = message(POLL);
        let expected = Poll::parse(&plain).unwrap().unwrap();
        for body in [
            "",
            "Translated body with completely different options",
            "📊 No body parsing allowed",
        ] {
            let m = message(&format!(
                "<body>{body}</body>{POLL}<fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:polls:0'><body/></fallback>"
            ));
            let p = Poll::parse(&m).unwrap().unwrap();
            assert_eq!(p, expected);
            assert!(p.accompanying_body(&m).is_empty());
        }
        let unmarked = message(&format!("<body>Accompanying text</body>{POLL}"));
        assert_eq!(expected.accompanying_body(&unmarked), "Accompanying text");
        let mut roundtrip = message("");
        roundtrip.append_child(expected.element());
        assert_eq!(Poll::parse(&roundtrip), Ok(Some(expected)));
    }
    #[test]
    fn rejects_ambiguous_unbounded_or_non_emoji_definitions() {
        for bad in [
            POLL.replace("single", "quiz"),
            POLL.replace("type='single'", "type='single' closed='yes'"),
            POLL.replace("</question>", "</question><question>Duplicate</question>"),
            POLL.replace("emoji='🍜'", "emoji='🍕'"),
            POLL.replace("emoji='🍜'", "emoji='hello'"),
            POLL.replace("Noodles", "<nested/>"),
            POLL.replace("Noodles", &"x".repeat(1025)),
            POLL.replace("Where 😀?", &"😀".repeat(4097)),
            format!("{POLL}{POLL}"),
        ] {
            assert!(Poll::parse(&message(&bad)).is_err());
        }
        let mut p = Poll::parse(&message(POLL)).unwrap().unwrap();
        p.question = "😀".repeat(4096);
        assert!(p.validate().is_ok());
        p.options = vec![
            OptionValue {
                emoji: "🍕".into(),
                label: "&".repeat(1024)
            };
            32
        ];
        assert!(p.validate().is_err());
    }
    #[test]
    fn freshly_compiled_mobile_wire_and_protected_payload_vectors() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let values: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../tests/mobile-reference/poll-vectors.json"))
                .unwrap();
        for value in values {
            let bytes = |key: &str| STANDARD.decode(value[key].as_str().unwrap()).unwrap();
            let string = |key: &str| String::from_utf8(bytes(key)).unwrap();
            let xml: Element = string("xml").parse().unwrap();
            let parsed = Poll::parse(&xml);
            if !value["valid"].as_bool().unwrap() {
                assert!(parsed.is_err());
                continue;
            }
            let poll = parsed.unwrap().unwrap();
            assert_eq!(poll.question, string("question"));
            assert_eq!(poll.description, string("description"));
            assert_eq!(poll.accompanying_body(&xml), string("body"));
            assert_eq!(poll.multiple, value["multiple"].as_bool().unwrap());
            assert_eq!(poll.closed, value["closed"].as_bool().unwrap());
            let expected: Vec<_> = value["options"]
                .as_array()
                .unwrap()
                .iter()
                .map(|pair| OptionValue {
                    emoji: String::from_utf8(STANDARD.decode(pair[0].as_str().unwrap()).unwrap())
                        .unwrap(),
                    label: String::from_utf8(STANDARD.decode(pair[1].as_str().unwrap()).unwrap())
                        .unwrap(),
                })
                .collect();
            assert_eq!(poll.options, expected);
            if value["authored"].as_bool().unwrap_or(false) {
                let (_, message, _) =
                    crate::protocol::normalize(&xml, "bob@example.org", None).unwrap();
                assert_eq!(message.poll.as_ref(), Some(&poll));
                if xml.get_child("reply", "urn:xmpp:reply:0").is_some() {
                    assert_eq!(message.text, "Authored note 😀");
                    assert_eq!(message.reply_id, "original");
                } else {
                    assert!(message.text.is_empty());
                    assert!(message.reply_id.is_empty());
                }
            }
            if value.get("sce").is_some() {
                let plain =
                    crate::crypto::payload::open(&bytes("ciphertext"), &bytes("material")).unwrap();
                assert_eq!(&*plain, &bytes("sce"));
                let content = crate::crypto::payload::content(&xml, &plain, 0).unwrap();
                assert_eq!(Poll::parse(&content).unwrap(), Some(poll.clone()));
            }
            if value.get("legacy_sce").is_some() {
                let plain = crate::crypto::payload::open(
                    &bytes("legacy_ciphertext"),
                    &bytes("legacy_material"),
                )
                .unwrap();
                assert_eq!(&*plain, &bytes("legacy_sce"));
                let content = crate::crypto::payload::content(&xml, &plain, 0).unwrap();
                assert_eq!(
                    content
                        .children()
                        .filter(|child| child.is("body", "jabber:client"))
                        .count(),
                    1
                );
                assert_eq!(poll.accompanying_body(&content), string("body"));
                assert_eq!(Poll::parse(&content).unwrap(), Some(poll));
            }
        }
    }
    #[test]
    fn bodyless_poll_is_authenticated_inside_sce() {
        let mut outer = message(POLL);
        for (name, value) in [
            ("from", "alice@example.org/phone"),
            ("to", "bob@example.org"),
            ("type", "chat"),
            ("id", "poll-123"),
        ] {
            outer.set_attr("".into(), name.try_into().unwrap(), value);
        }
        let expected = Poll::parse(&outer).unwrap().unwrap();
        let plain = crate::crypto::payload::envelope(&outer).unwrap();
        let sealed = crate::crypto::payload::seal(&plain).unwrap();
        let decoded = crate::crypto::payload::content(
            &outer,
            &crate::crypto::payload::open(&sealed.ciphertext, &sealed.material).unwrap(),
            0,
        )
        .unwrap();
        assert_eq!(Poll::parse(&decoded).unwrap(), Some(expected));
        assert!(decoded.get_child("body", "jabber:client").is_none());
    }
    #[test]
    fn authored_fields_match_the_serialized_xml_before_identity_and_closure_checks() {
        let mut poll = Poll::parse(&message(POLL)).unwrap().unwrap();
        poll.question = "Question\r\nwith & <xml>".into();
        poll.options[0].label = "Choice\rwith detail".into();
        let canonical = poll.canonicalized().unwrap();
        assert_eq!(canonical.canonicalized().unwrap(), canonical);
        let mut wire = message("");
        wire.append_child(canonical.element());
        let mut bytes = Vec::new();
        wire.write_to(&mut bytes).unwrap();
        let received: Element = String::from_utf8(bytes).unwrap().parse().unwrap();
        assert_eq!(Poll::parse(&received).unwrap(), Some(canonical));
    }
    #[test]
    fn byte_budget_is_independent_from_field_and_option_count_bounds() {
        let options: Vec<_> = include_str!("../assets/reaction-emoji.tsv")
            .lines()
            .take(32)
            .map(|line| OptionValue {
                emoji: line.split_once('\t').unwrap().1.into(),
                label: "中".repeat(1024),
            })
            .collect();
        let poll = Poll {
            question: "Question".into(),
            description: String::new(),
            options,
            multiple: false,
            closed: false,
        };
        assert_eq!(
            poll.options
                .iter()
                .map(|o| &o.emoji)
                .collect::<BTreeSet<_>>()
                .len(),
            32
        );
        assert!(poll.validate().is_err());
        assert!(
            Poll::parse(&message(
                &POLL.replace("🍕", &format!("🍕{}", "\u{fe0f}".repeat(128)))
            ))
            .is_err()
        );
    }
    #[test]
    fn ballot_sets_count_people_preserve_other_reactions_and_follow_explicit_mapping() {
        let mut poll = Poll::parse(&message(POLL)).unwrap().unwrap();
        poll.multiple = true;
        let sets = [
            (false, ["🍕".into(), "🍜".into(), "❤️".into()].into()),
            (true, ["🍜".into()].into()),
        ]
        .into();
        let result = poll.tally(&sets, "own", "peer");
        assert_eq!(result.counts, vec![1, 2]);
        assert_eq!(result.ballots.len(), 2);
        assert_eq!(result.chosen, [1].into());
    }
    #[test]
    fn outgoing_definition_has_optional_body_and_unicode_reply_fallbacks_are_independent() {
        use crate::model::Message;
        let poll = Poll::parse(&message(POLL)).unwrap().unwrap();
        for fallback in [false, true] {
            let mut m = Message {
                poll: Some(poll.clone()),
                poll_fallback: fallback,
                wire_id: "wire".into(),
                ..Default::default()
            };
            let mut xml = crate::protocol::outgoing("bob@example.org", &m, "");
            assert_eq!(xml.get_child("body", "jabber:client").is_some(), fallback);
            xml.set_attr(
                "".into(),
                "from".try_into().unwrap(),
                "alice@example.org/phone",
            );
            let (_, decoded, _) =
                crate::protocol::normalize(&xml, "bob@example.org", None).unwrap();
            assert_eq!(decoded.poll, Some(poll.clone()));
            assert!(decoded.text.is_empty());
            m.reply_id = "reply".into();
            m.reply_to = "bob@example.org".into();
            let mut xml = crate::protocol::outgoing("bob@example.org", &m, "Quoted 🦊");
            xml.set_attr(
                "".into(),
                "from".try_into().unwrap(),
                "alice@example.org/phone",
            );
            let (_, decoded, _) =
                crate::protocol::normalize(&xml, "bob@example.org", None).unwrap();
            assert_eq!(decoded.reply_id, "reply");
            assert!(decoded.text.is_empty());
            m.text = "Accompanying 🦊 text".into();
            let mut xml = crate::protocol::outgoing("bob@example.org", &m, "Quoted 🦊");
            xml.set_attr(
                "".into(),
                "from".try_into().unwrap(),
                "alice@example.org/phone",
            );
            let (_, decoded, _) =
                crate::protocol::normalize(&xml, "bob@example.org", None).unwrap();
            assert_eq!(decoded.text, "Accompanying 🦊 text");
        }
    }
    #[test]
    fn fallback_ranges_are_code_points_and_closure_cannot_change_definition() {
        let p = Poll::parse(&message(POLL)).unwrap().unwrap();
        for (range, expected) in [
            ("start='0' end='2'", "after"),
            ("start='0' end='99'", "😀 after"),
            ("start='1'", "😀 after"),
        ] {
            let m = message(&format!(
                "<body>😀 after</body>{POLL}<fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:polls:0'><body {range}/></fallback>"
            ));
            assert_eq!(p.accompanying_body(&m), expected);
        }
        let mut closed = p.clone();
        closed.closed = true;
        assert!(p.same_definition(&closed));
        closed.options.reverse();
        assert!(!p.same_definition(&closed));
    }
}
