//! Mobile compatibility profile 1, ported from ForwardedContent.java (GPL-3.0-or-later).
//! Attribution is the forwarder's claim and never supplies live stanza authority.
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tokio_xmpp::{jid::Jid, minidom::Element};
pub const NS: &str = "urn:xmpp:forward:0";
const CLIENT: &str = "jabber:client";
const PREFIX: &str = "↪ Forwarded from ";
const SEP: &str = " · ";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribution {
    pub name: String,
    pub date: i32,
}
fn unsafe_label(c: char) -> bool {
    c.is_control() || matches!(c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')
}
pub fn address(value: &str) -> Option<String> {
    if value.encode_utf16().count() > 3071 || value.chars().any(unsafe_label) {
        return None;
    }
    let jid: Jid = value.parse().ok()?;
    jid.node()?;
    Some(jid.to_string())
}
impl Attribution {
    pub fn new(name: &str, date: i32) -> Option<Self> {
        let name = address(name).unwrap_or_else(|| {
            name.chars()
                .filter(|c| !unsafe_label(*c))
                .take(128)
                .collect::<String>()
                .trim_matches(|c| c <= '\u{20}')
                .to_string()
        });
        (!name.is_empty()).then_some(Self {
            name,
            date: date.max(0),
        })
    }
    pub fn stamp(&self) -> String {
        Utc.timestamp_opt(i64::from(self.date.max(0)), 0)
            .single()
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }
    pub fn prefix(&self) -> String {
        format!("{PREFIX}{}{SEP}{}\n\n", self.name, self.stamp())
    }
    pub fn wire_body(&self, body: &str) -> String {
        format!("{}{body}", self.prefix())
    }
}
pub fn parse(body: &str) -> Option<(Attribution, String)> {
    if !body.starts_with(PREFIX) {
        return None;
    }
    let end = body.find("\n\n")?;
    if body[..end].encode_utf16().count() > 4096 {
        return None;
    }
    let (name, stamp) = body[PREFIX.len()..end].rsplit_once(SEP)?;
    let seconds = DateTime::parse_from_rfc3339(stamp).ok()?.timestamp();
    if !(0..=i64::from(i32::MAX)).contains(&seconds) {
        return None;
    }
    let attribution = Attribution::new(name, seconds as i32)?;
    let content = &body[end + 2..];
    (attribution.wire_body(content) == body).then(|| (attribution, content.to_string()))
}
/// Read just one quoted message. Its sender/IDs/extensions never reach stanza processing.
pub fn standalone(message: &Element) -> Option<(Attribution, String)> {
    let mut forwards = message.children().filter(|c| c.is("forwarded", NS));
    let forward = forwards.next()?;
    if forwards.next().is_some() {
        return None;
    }
    let mut originals = forward.children().filter(|c| c.is("message", CLIENT));
    let original = originals.next()?;
    if originals.next().is_some() || original.get_child("forwarded", NS).is_some() {
        return None;
    }
    let xml_language = "http://www.w3.org/XML/1998/namespace";
    let bodies: Vec<_> = original
        .children()
        .filter(|c| c.is("body", CLIENT))
        .collect();
    let language = original
        .attr_ns(xml_language, "lang")
        .map(str::to_string)
        .unwrap_or_else(|| {
            let locale = gtk::glib::language_names()
                .first()
                .map(|s| s.to_string())
                .unwrap_or_default();
            let language = locale.split(['_', '-', '.']).next().unwrap_or("");
            if language.is_empty() || language == "C" {
                "en".into()
            } else {
                language.to_ascii_lowercase()
            }
        });
    let body = bodies
        .iter()
        .find(|body| body.attr_ns(xml_language, "lang").is_none())
        .or_else(|| {
            bodies.iter().find(|body| {
                body.attr_ns(xml_language, "lang")
                    .is_some_and(|lang| lang.eq_ignore_ascii_case(&language))
            })
        })?
        .text();
    let mut delays = forward
        .children()
        .filter(|c| c.is("delay", "urn:xmpp:delay"));
    let delay = delays.next();
    if delays.next().is_some() {
        return None;
    }
    let seconds = if let Some(delay) = delay {
        DateTime::parse_from_rfc3339(delay.attr("stamp")?)
            .ok()?
            .timestamp()
    } else {
        0
    };
    if !(0..=i64::from(i32::MAX)).contains(&seconds) {
        return None;
    }
    Some((
        Attribution::new(original.attr("from")?, seconds as i32)?,
        body,
    ))
}
pub fn key_uri(body: &str) -> bool {
    body.trim_start()
        .get(..9)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("aesgcm://"))
}
pub fn original(message: &crate::model::Message, owner: &str, group: bool) -> Attribution {
    if let Some(attribution) = &message.forwarded {
        return attribution.clone();
    }
    let known = if message.own && !owner.is_empty() {
        address(owner)
    } else {
        address(&message.sender)
    };
    let name = known
        .map(|name| {
            if group {
                name
            } else {
                name.parse::<Jid>().unwrap().to_bare().to_string()
            }
        })
        .unwrap_or_else(|| {
            if message.author.is_empty() {
                "Unknown sender".into()
            } else {
                message.author.clone()
            }
        });
    let date = DateTime::parse_from_rfc3339(&message.timestamp)
        .ok()
        .and_then(|d| i32::try_from(d.timestamp()).ok())
        .unwrap_or(0);
    Attribution::new(&name, date)
        .unwrap_or_else(|| Attribution::new("Unknown sender", date).unwrap())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::Message, polls::Source};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    #[test]
    fn actual_mobile_wire_models_and_protected_forward_vectors() {
        let vectors: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../tests/mobile-reference/forward-vectors.json"
        ))
        .unwrap();
        for (index, value) in vectors.into_iter().enumerate() {
            let bytes = |key: &str| STANDARD.decode(value[key].as_str().unwrap()).unwrap();
            let string = |key: &str| String::from_utf8(bytes(key)).unwrap();
            let xml: Element = string("xml").parse().unwrap();
            let (peer, message, _) =
                crate::protocol::normalize(&xml, "bob@example.org", None).unwrap();
            assert_eq!(peer, "alice@example.org");
            assert_eq!(message.sender, "alice@example.org/forwarder");
            assert_eq!(message.wire_id, "outer");
            assert!(message.action.is_none());
            assert!(!message.aliases.iter().any(|id| id.contains("forged")));
            if value["forwarded"].as_bool().unwrap() {
                let expected = Attribution {
                    name: string("name"),
                    date: value["date"].as_i64().unwrap() as i32,
                };
                assert_eq!(message.forwarded, Some(expected.clone()));
                assert_eq!(message.text, string("payload"));
                assert_eq!(expected.wire_body(&message.text), string("body"));
            } else {
                assert!(
                    message.forwarded.is_none(),
                    "Unexpected attribution in mobile vector {index}"
                );
                assert_eq!(message.text, string("body"));
            }
            let restored = Message::decode(&serde_json::to_string(&message).unwrap()).unwrap();
            assert_eq!(restored.forwarded, message.forwarded);
            assert_eq!(restored.text, message.text);
            if value.get("sce").is_some() {
                let plain =
                    crate::crypto::payload::open(&bytes("ciphertext"), &bytes("material")).unwrap();
                assert_eq!(&*plain, &bytes("sce"));
                let content = crate::crypto::payload::content(&xml, &plain, 0).unwrap();
                let (_, protected, _) =
                    crate::protocol::normalize(&content, "bob@example.org", None).unwrap();
                assert_eq!(protected.forwarded, message.forwarded);
                assert_eq!(protected.text, message.text);
                assert!(protected.action.is_none());
            }
        }
    }
    #[test]
    fn original_identity_and_unknown_date_survive_reforwarding_without_guessing() {
        let mut message = Message {
            sender: "original@example.org/device".into(),
            author: "A local nickname".into(),
            timestamp: "2023-11-14T22:13:20Z".into(),
            ..Default::default()
        };
        assert_eq!(
            original(&message, "owner@example.org", false).name,
            "original@example.org"
        );
        assert_eq!(
            original(&message, "owner@example.org", true).name,
            "original@example.org/device"
        );
        message.forwarded = Attribution::new("Original label", 0);
        let claimed = original(&message, "owner@example.org", false);
        assert_eq!(claimed.name, "Original label");
        assert_eq!(claimed.date, 0);
        message.forwarded = None;
        message.own = true;
        assert_eq!(
            original(&message, "owner@example.org", false).name,
            "owner@example.org"
        );
    }
    #[test]
    fn prefixed_legacy_polls_recover_and_keep_attribution_on_closure() {
        let wire: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../tests/mobile-reference/legacy-poll-vectors.json"
        ))
        .unwrap();
        let body =
            String::from_utf8(STANDARD.decode(wire[0]["body"].as_str().unwrap()).unwrap()).unwrap();
        let attribution = Attribution::new("original@example.org/device", 1700000000).unwrap();
        let mut original = Message {
            text: attribution.wire_body(&body),
            poll_source: Source::Text,
            ..Default::default()
        };
        original = Message::decode(&serde_json::to_string(&original).unwrap()).unwrap();
        assert_eq!(original.poll_source(), Source::Legacy);
        assert_eq!(original.forwarded, Some(attribution.clone()));
        assert_eq!(original.text, body);
        let mut closed = original.clone();
        closed.poll.as_mut().unwrap().closed = true;
        closed.text = crate::legacy_polls::body(closed.poll.as_ref().unwrap()).unwrap();
        let mut xml = crate::protocol::outgoing("bob@example.org", &closed, "");
        xml.set_attr(
            "".into(),
            "from".try_into().unwrap(),
            "alice@example.org/forwarder",
        );
        let (_, received, _) = crate::protocol::normalize(&xml, "bob@example.org", None).unwrap();
        assert_eq!(received.forwarded, Some(attribution));
        assert_eq!(received.poll_source(), Source::Legacy);
        assert!(received.poll.unwrap().closed);
    }
    #[test]
    fn outer_unprotected_claims_cannot_become_authenticated_forward_metadata() {
        let clear:Element="<message xmlns='jabber:client' from='alice@example.org/forwarder' to='bob@example.org' type='chat' id='outer'><body>Private text</body></message>".parse().unwrap();
        let mut outer = clear.clone();
        outer.append_child("<forwarded xmlns='urn:xmpp:forward:0'><message xmlns='jabber:client' from='forged@example.org' id='forged'><body>Forged body</body></message></forwarded>".parse::<Element>().unwrap());
        let plain = crate::crypto::payload::envelope(&clear).unwrap();
        let protected = crate::crypto::payload::content(&outer, &plain, 0).unwrap();
        let (_, message, _) =
            crate::protocol::normalize(&protected, "bob@example.org", None).unwrap();
        assert!(message.forwarded.is_none());
        assert_eq!(message.text, "Private text");
        assert_eq!(message.sender, "alice@example.org/forwarder");
    }
    #[test]
    fn encrypted_file_references_are_not_forwardable_text() {
        for body in [
            "aesgcm://files.example.org/private#test",
            "AESGCM://files.example.org/private#test",
        ] {
            let mut message = Message {
                text: Attribution::new("Original", 0).unwrap().wire_body(body),
                ..Default::default()
            };
            message.hydrate_compatibility();
            assert!(message.unsupported_attachment);
            assert_eq!(message.text, body);
        }
    }
}
