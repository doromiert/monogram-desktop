//! Mobile-compatible direct-message controls and deterministic, durable projections.
use crate::model::Message;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};
use tokio_xmpp::minidom::Element;
pub const CORRECT: &str = "urn:xmpp:message-correct:0";
pub const REACTIONS: &str = "urn:xmpp:reactions:0";
pub const RETRACT: &str = "urn:xmpp:message-retract:1";
pub const OLD_RETRACT: &str = "urn:xmpp:message-retract:0";
const FASTEN: &str = "urn:xmpp:fasten:0";
const FALLBACK: &str = "urn:xmpp:fallback:0";
const CLIENT: &str = "jabber:client";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Correct {
        target: String,
    },
    React {
        target: String,
        emojis: BTreeSet<String>,
    },
    Retract {
        target: String,
        legacy: bool,
    },
}
impl Action {
    pub fn target(&self) -> &str {
        match self {
            Self::Correct { target }
            | Self::React { target, .. }
            | Self::Retract { target, .. } => target,
        }
    }
}
pub fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.encode_utf16().count() <= 4096
}
fn empty(element: &Element) -> bool {
    element.children().next().is_none() && element.text().trim_matches(|c| c <= '\u{20}').is_empty()
}
fn one<'a>(xml: &'a Element, name: &str, ns: &str) -> Result<Option<&'a Element>, ()> {
    let mut found = xml.children().filter(|c| c.is(name, ns));
    let first = found.next();
    if found.next().is_some() {
        Err(())
    } else {
        Ok(first)
    }
}
fn target(element: &Element) -> Result<String, ()> {
    let id = element.attr("id").filter(|v| valid_id(v)).ok_or(())?;
    Ok(id.into())
}
pub fn parse(xml: &Element) -> Result<Option<Action>, ()> {
    let correction = one(xml, "replace", CORRECT)
        .ok()
        .flatten()
        .filter(|e| empty(e))
        .and_then(|e| target(e).ok());
    let current = one(xml, "retract", RETRACT)?;
    let mut legacy = None;
    for wrapper in xml.children().filter(|c| c.is("apply-to", FASTEN)) {
        if wrapper.get_child("retract", OLD_RETRACT).is_some() {
            if legacy.is_some() || wrapper.children().count() != 1 {
                return Err(());
            }
            legacy = Some(wrapper);
        }
    }
    let retraction = if let Some(e) = current {
        if legacy.is_some() || !empty(e) {
            return Err(());
        }
        Some(Action::Retract {
            target: target(e)?,
            legacy: false,
        })
    } else if let Some(e) = legacy {
        if !empty(e.children().next().unwrap()) {
            return Err(());
        }
        Some(Action::Retract {
            target: target(e)?,
            legacy: true,
        })
    } else {
        None
    };
    if let Some(reactions) = one(xml, "reactions", REACTIONS)? {
        if correction.is_some()
            || retraction.is_some()
            || !reactions.text().trim_matches(|c| c <= '\u{20}').is_empty()
            || reactions.children().count() > 32
            || xml.get_child("file-sharing", "urn:xmpp:sfs:0").is_some()
        {
            return Err(());
        }
        let id = target(reactions)?;
        let mut emojis = BTreeSet::new();
        for e in reactions.children() {
            let emoji = e.text();
            if !e.is("reaction", REACTIONS)
                || e.children().next().is_some()
                || emoji.is_empty()
                || emoji.encode_utf16().count() > 128
                || emoji.chars().any(|c| {
                    c.is_control()
                        || c.is_whitespace()
                            && !matches!(c, '\u{85}' | '\u{a0}' | '\u{2007}' | '\u{202f}')
                })
            {
                return Err(());
            }
            emojis.insert(emoji);
        }
        if let Some(reply) = one(xml, "reply", "urn:xmpp:reply:0")? {
            if reply.attr("id") != Some(&id) {
                return Err(());
            }
        }
        let body = xml
            .get_child("body", CLIENT)
            .map(|b| b.text())
            .unwrap_or_default();
        if !body.is_empty() {
            let valid = xml
                .children()
                .filter(|f| f.is("fallback", FALLBACK) && f.attr("for") == Some(REACTIONS))
                .any(|f| {
                    let mut children = f.children();
                    let Some(text) = children.next() else {
                        return false;
                    };
                    children.next().is_none()
                        && text.is("body", FALLBACK)
                        && text.children().next().is_none()
                        && text
                            .attr_ns("http://www.w3.org/XML/1998/namespace", "lang")
                            .is_none()
                        && ((text.attr("start").is_none() && text.attr("end").is_none())
                            || (text.attr("start") == Some("0")
                                && text.attr("end")
                                    == Some(body.chars().count().to_string().as_str())))
                });
            if !valid {
                return Err(());
            }
        }
        return Ok(Some(Action::React { target: id, emojis }));
    }
    if retraction.is_some() {
        return Ok(retraction);
    }
    Ok(correction.map(|target| Action::Correct { target }))
}
pub fn canonical_emoji(value: &str) -> Option<String> {
    static VALUES: OnceLock<BTreeMap<&'static str, &'static str>> = OnceLock::new();
    let values = VALUES.get_or_init(|| {
        include_str!("../assets/reaction-emoji.tsv")
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .collect()
    });
    values
        .get(value.replace('\u{fe0f}', "").as_str())
        .map(|v| (*v).into())
}
pub fn decorate(root: &mut Element, action: &Action) {
    match action {
        Action::Correct { target } => {
            root.append_child(
                Element::builder("replace", CORRECT)
                    .attr("id".try_into().unwrap(), target)
                    .build(),
            );
        }
        Action::React { target, emojis } => {
            let nodes = root.take_nodes();
            for node in nodes {
                if node.as_element().is_none_or(|e| {
                    !e.is("body", CLIENT)
                        && !e.is("reply", "urn:xmpp:reply:0")
                        && !e.is("fallback", FALLBACK)
                }) {
                    root.append_node(node);
                }
            }
            let mut set = Element::builder("reactions", REACTIONS)
                .attr("id".try_into().unwrap(), target)
                .build();
            for emoji in emojis {
                set.append_child(
                    Element::builder("reaction", REACTIONS)
                        .append(emoji.as_str())
                        .build(),
                );
            }
            root.append_child(set);
        }
        Action::Retract { target, legacy } => {
            let ns = if *legacy { OLD_RETRACT } else { RETRACT };
            let mut retract = Element::builder("retract", ns).build();
            if *legacy {
                root.append_child(
                    Element::builder("apply-to", FASTEN)
                        .attr("id".try_into().unwrap(), target)
                        .append(retract)
                        .build(),
                );
            } else {
                retract.set_attr("".into(), "id".try_into().unwrap(), target);
                root.append_child(retract);
            }
            root.append_child(
                Element::builder("fallback", FALLBACK)
                    .attr("for".try_into().unwrap(), ns)
                    .build(),
            );
        }
    }
    if root.get_child("store", "urn:xmpp:hints").is_none() {
        root.append_child(Element::builder("store", "urn:xmpp:hints").build());
    }
}
pub fn source_key(message: &Message) -> String {
    message
        .aliases
        .iter()
        .find(|v| v.starts_with("wire:"))
        .or_else(|| message.aliases.iter().find(|v| v.starts_with("omemo2:")))
        .or_else(|| message.aliases.first())
        .cloned()
        .unwrap_or_else(|| format!("local:{}", message.id))
}
pub fn original_wire(message: &Message) -> Option<String> {
    valid_id(&message.wire_id).then(|| message.wire_id.clone())
}
pub fn reaction_reference(message: &Message) -> Option<String> {
    message
        .aliases
        .iter()
        .find_map(|v| {
            v.strip_prefix("origin:")
                .filter(|v| valid_id(v))
                .map(str::to_string)
        })
        .or_else(|| original_wire(message))
}
fn match_wire(message: &Message, target: &str) -> bool {
    message.wire_id == target
        || message
            .aliases
            .iter()
            .any(|v| v.strip_prefix("wire:") == Some(target))
}
fn match_reaction(message: &Message, target: &str) -> bool {
    match_wire(message, target)
        || message
            .aliases
            .iter()
            .any(|v| v.strip_prefix("origin:") == Some(target))
}
#[derive(Clone, Debug)]
pub struct Event {
    pub row: i64,
    pub message: Message,
    pub failed: bool,
    pub resolved: Option<String>,
}
pub struct Projection {
    pub messages: Vec<Message>,
    pub events: Vec<Event>,
}
pub fn project(
    mut bases: Vec<Message>,
    mut events: Vec<Event>,
    owner: &str,
    peer: &str,
) -> Projection {
    for base in &mut bases {
        base.hydrate_compatibility();
    }
    for event in &mut events {
        event.message.hydrate_compatibility();
    }
    events.sort_by(|a, b| {
        a.message
            .timestamp
            .cmp(&b.message.timestamp)
            .then(a.row.cmp(&b.row))
    });
    let mut floors: BTreeMap<String, bool> =
        bases.iter().map(|m| (m.id.clone(), m.encrypted)).collect();
    let mut rendered: BTreeMap<String, Message> =
        bases.iter().cloned().map(|m| (m.id.clone(), m)).collect();
    for event in &mut events {
        event.resolved = None;
        if event.failed {
            continue;
        }
        let m = &event.message;
        let Some(Action::Correct { target }) = &m.action else {
            continue;
        };
        let candidates: Vec<_> = bases
            .iter()
            .filter(|base| base.own == m.own && match_wire(base, target) && !base.unavailable)
            .collect();
        if candidates.len() != 1 {
            continue;
        }
        let base = candidates[0];
        // Caption edits need an unchanged attachment identity; text replacement is insufficient.
        if base.attachment.is_some() || m.attachment.is_some() {
            continue;
        }
        if *floors.get(&base.id).unwrap() && !m.encrypted {
            continue;
        }
        match (&base.poll, &m.poll) {
            (Some(original), Some(edit))
                if edit.closed
                    && original.same_definition(edit)
                    && base.poll_source() == m.poll_source() => {}
            (None, None)
                if base.poll_source() != crate::polls::Source::Structured
                    && m.poll_source() != crate::polls::Source::Structured => {}
            _ => continue,
        }
        event.resolved = Some(base.id.clone());
        if base.retracted {
            continue;
        }
        floors.insert(base.id.clone(), m.encrypted);
        let value = rendered.get_mut(&base.id).unwrap();
        if !value.edited {
            value.edit_history.push(value.content_text());
        }
        if m.poll.as_ref().is_some_and(|poll| poll.closed) && value.poll_closed_at.is_empty() {
            value.poll_closed_at = if base.poll.as_ref().is_some_and(|poll| poll.closed) {
                base.timestamp.clone()
            } else {
                m.timestamp.clone()
            };
        }
        value.poll = m.poll.clone();
        value.poll_source = m.poll_source;
        value.text = m.text.clone();
        value.forwarded = m.forwarded.clone();
        value.unsupported_attachment = m.unsupported_attachment;
        value.reply = m.reply.clone();
        value.reply_id = m.reply_id.clone();
        value.reply_to = m.reply_to.clone();
        value.encrypted = m.encrypted;
        value.fingerprint = m.fingerprint.clone();
        value.edited = true;
        value.edit_history.push(m.content_text());
    }
    for event in &mut events {
        if event.failed {
            continue;
        }
        let m = &event.message;
        let Some(Action::Retract { target, legacy }) = &m.action else {
            continue;
        };
        let candidates: Vec<_> = bases
            .iter()
            .filter(|base| {
                base.own == m.own
                    && !base.unavailable
                    && if *legacy {
                        base.aliases
                            .iter()
                            .any(|v| v.strip_prefix("origin:") == Some(target))
                    } else {
                        match_wire(base, target)
                    }
            })
            .collect();
        if candidates.len() != 1 {
            continue;
        }
        let base = candidates[0];
        if base.encrypted
            && (base.mode() != m.mode()
                || base.fingerprint.is_empty()
                || base.fingerprint != m.fingerprint)
        {
            continue;
        }
        event.resolved = Some(base.id.clone());
        let value = rendered.get_mut(&base.id).unwrap();
        value.retracted = true;
    }
    let mut vote_latest: BTreeMap<(String, bool), (bool, BTreeSet<String>)> = BTreeMap::new();
    let mut latest: BTreeMap<(String, bool), (String, bool, BTreeSet<String>, i64)> =
        BTreeMap::new();
    let correction_refs: Vec<_> = events
        .iter()
        .filter_map(|e| {
            e.resolved
                .as_ref()
                .filter(|_| matches!(e.message.action, Some(Action::Correct { .. })))
                .map(|id| (e.message.clone(), id.clone()))
        })
        .collect();
    for event in &mut events {
        if event.failed {
            continue;
        }
        let m = &event.message;
        let Some(Action::React { target, emojis }) = &m.action else {
            continue;
        };
        let mut candidates: BTreeSet<_> = bases
            .iter()
            .filter(|base| !base.unavailable && match_reaction(base, target))
            .map(|base| base.id.clone())
            .collect();
        for (correction, id) in &correction_refs {
            if match_reaction(correction, target) {
                candidates.insert(id.clone());
            }
        }
        if candidates.len() != 1 {
            continue;
        }
        let id = candidates.pop_first().unwrap();
        event.resolved = Some(id.clone());
        let key = (id, m.own);
        if latest.get(&key).is_some_and(|v| v.1 && !m.encrypted) {
            continue;
        }
        let known: BTreeSet<String> = emojis.iter().filter_map(|e| canonical_emoji(e)).collect();
        let value = rendered.get(&key.0).unwrap();
        let original = bases.iter().find(|m| m.id == key.0).unwrap();
        let cutoff = if value.poll.as_ref().is_some_and(|p| p.closed) {
            if value.poll_closed_at.is_empty() {
                &original.timestamp
            } else {
                &value.poll_closed_at
            }
        } else {
            "~"
        };
        if value.poll.is_some()
            && m.timestamp.as_str() <= cutoff
            && (!original.encrypted || m.encrypted)
            && !vote_latest.get(&key).is_some_and(|v| v.0 && !m.encrypted)
        {
            vote_latest.insert(key.clone(), (m.encrypted, known.clone()));
        }
        latest.insert(
            key,
            (
                if m.own { owner } else { peer }.into(),
                m.encrypted,
                known,
                event.row,
            ),
        );
    }
    for value in rendered.values_mut() {
        value.reactions.clear();
        value.my_reactions.clear();
        if value.retracted {
            scrub(value);
            continue;
        }
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        for ((id, own), (_, _, emojis, _)) in &latest {
            if id == &value.id {
                for emoji in emojis {
                    *counts.entry(emoji.clone()).or_default() += 1;
                }
                if *own {
                    value.my_reactions = emojis.clone();
                }
            }
        }
        if let Some(poll) = &value.poll {
            let sets = vote_latest
                .iter()
                .filter(|((id, _), _)| id == &value.id)
                .map(|((_, own), (_, emojis))| (*own, emojis.clone()))
                .collect();
            value.poll_tally = Some(poll.tally(&sets, owner, peer));
        }
        value.reactions = counts
            .into_iter()
            .map(|(e, n)| format!("{e} {n}"))
            .collect();
    }
    let mut output: Vec<_> = rendered.into_values().collect();
    for event in &events {
        if !event.failed
            && event.resolved.is_none()
            && matches!(event.message.action, Some(Action::Correct { .. }))
        {
            let mut visible = event.message.clone();
            if visible.poll.is_some() {
                visible.text = visible.content_text();
                visible.poll = None;
                visible.poll_source = crate::polls::Source::Text;
                visible.poll_tally = None;
                visible.kind = "text".into();
            }
            output.push(visible);
        }
    }
    output.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.id.cmp(&b.id)));
    Projection {
        messages: output,
        events,
    }
}
pub fn scrub(message: &mut Message) {
    message.attachment = None;
    message.forwarded = None;
    message.unsupported_attachment = false;
    message.retracted = true;
    message.text.clear();
    message.poll = None;
    message.poll_source = crate::polls::Source::Text;
    message.poll_tally = None;
    message.poll_closed_at.clear();
    message.ciphertext.clear();
    message.reply.clear();
    message.reply_id.clear();
    message.reply_to.clear();
    message.reactions.clear();
    message.my_reactions.clear();
    message.edit_history.clear();
    message.unread = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse_xml(content: &str) -> Result<Option<Action>, ()> {
        parse(
            &format!("<message xmlns='jabber:client'>{content}</message>")
                .parse::<Element>()
                .unwrap(),
        )
    }
    #[test]
    fn grammar_matches_mobile_controls() {
        assert_eq!(
            parse_xml("<body>new</body><replace xmlns='urn:xmpp:message-correct:0' id='a'/>"),
            Ok(Some(Action::Correct { target: "a".into() }))
        );
        assert_eq!(
            parse_xml(
                "<body>ordinary</body><replace xmlns='urn:xmpp:message-correct:0' id='a'/><replace xmlns='urn:xmpp:message-correct:0' id='b'/>"
            ),
            Ok(None)
        );
        assert!(parse_xml("<reactions xmlns='urn:xmpp:reactions:0' id='a'><reaction>👍</reaction><reaction>👍</reaction></reactions>").is_ok());
        assert!(parse_xml("<reactions xmlns='urn:xmpp:reactions:0' id='a'/>").is_ok());
        assert!(
            parse_xml(
                "<body>unmarked fallback</body><reactions xmlns='urn:xmpp:reactions:0' id='a'/>"
            )
            .is_err()
        );
        assert!(parse_xml("<body>😀 hi</body><reactions xmlns='urn:xmpp:reactions:0' id='a'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reactions:0'><body start='0' end='4'/></fallback>").is_ok());
        assert!(parse_xml("<reactions xmlns='urn:xmpp:reactions:0' id='a'><reaction>two words</reaction></reactions>").is_err());
        assert!(
            parse_xml("<retract xmlns='urn:xmpp:message-retract:1' id='a'><body/></retract>")
                .is_err()
        );
        assert!(parse_xml("<retract xmlns='urn:xmpp:message-retract:1' id='a'/><apply-to xmlns='urn:xmpp:fasten:0' id='a'><retract xmlns='urn:xmpp:message-retract:0'/></apply-to>").is_err());
        assert_eq!(canonical_emoji("❤"), Some("❤️".into()));
        assert_eq!(canonical_emoji("👍🏽"), Some("👍🏽".into()));
        assert_eq!(canonical_emoji("hello"), None);
        assert_eq!(
            parse_xml(
                "<body>ordinary</body><replace xmlns='urn:xmpp:message-correct:0' id='a'> </replace>"
            ),
            Ok(None)
        );
    }
    #[test]
    fn outgoing_controls_roundtrip_without_exposing_reaction_body() {
        for action in [
            Action::Correct {
                target: "first".into(),
            },
            Action::React {
                target: "first".into(),
                emojis: ["❤️".into()].into(),
            },
            Action::Retract {
                target: "first".into(),
                legacy: false,
            },
            Action::Retract {
                target: "first".into(),
                legacy: true,
            },
        ] {
            let m = Message {
                action: Some(action.clone()),
                text: "new".into(),
                wire_id: "event".into(),
                ..Default::default()
            };
            let mut xml = crate::protocol::outgoing("b@example.org", &m, "");
            xml.set_attr(
                "".into(),
                "from".try_into().unwrap(),
                "a@example.org/device",
            );
            let (_, decoded, _) = crate::protocol::normalize(&xml, "b@example.org", None).unwrap();
            assert_eq!(decoded.action, Some(action.clone()));
            if matches!(action, Action::React { .. } | Action::Retract { .. }) {
                assert!(decoded.text.is_empty());
            }
        }
    }
}
