//! Live XEP-0045 mediated and XEP-0249 direct invitations; receipt never joins a room.
use super::{Password, addressed_to, bounded};
use serde::{Deserialize, Serialize};
use std::fmt;
use tokio_xmpp::{jid::Jid, minidom::Element};

pub const DIRECT: &str = "jabber:x:conference";
const USER: &str = "http://jabber.org/protocol/muc#user";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Mediated,
    Direct,
}
impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mediated => "mediated",
            Self::Direct => "direct",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "mediated" => Some(Self::Mediated),
            "direct" => Some(Self::Direct),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct Incoming {
    pub room: String,
    pub inviter: String,
    pub reason: String,
    pub kind: Kind,
    pub password: Option<Password>,
}
impl fmt::Debug for Incoming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IncomingInvitation")
            .field("room", &self.room)
            .field("inviter", &self.inviter)
            .field("reason", &self.reason)
            .field("kind", &self.kind)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// Recognized invalid invitations must not fall back to rendering their ordinary body.
pub fn recognized(message: &Element) -> bool {
    message.children().any(|child| {
        child.is("x", DIRECT) || child.is("x", USER) && child.get_child("invite", USER).is_some()
    })
}

fn entity(value: &str, require_bare: bool) -> Option<String> {
    if value.encode_utf16().count() > 3071 {
        return None;
    }
    let jid = value.parse::<Jid>().ok()?;
    if jid.to_bare().node().is_none() || require_bare && jid.resource().is_some() {
        return None;
    }
    Some(jid.to_string())
}

fn unique<'a>(xml: &'a Element, name: &str, ns: &str) -> Option<Option<&'a Element>> {
    let mut matching = xml.children().filter(|child| child.is(name, ns));
    let first = matching.next();
    matching.next().is_none().then_some(first)
}

fn reason(value: &str) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|character| {
            units += character.len_utf16();
            units <= 512
        })
        .collect()
}

pub fn parse(message: &Element, owner: &str) -> Option<Incoming> {
    if !message.is("message", super::CLIENT)
        || !matches!(message.attr("type"), None | Some("normal" | "chat"))
        || !addressed_to(message, owner)
        || bounded(message).is_err()
        || message.children().any(|child| {
            matches!(
                child.ns().as_ref(),
                "urn:xmpp:delay"
                    | "jabber:x:delay"
                    | "urn:xmpp:forward:0"
                    | "urn:xmpp:carbons:2"
                    | "urn:xmpp:mam:1"
                    | "urn:xmpp:mam:2"
            )
        })
    {
        return None;
    }
    let direct = unique(message, "x", DIRECT)?;
    let mediated = unique(message, "x", USER)?;
    let (room, inviter, reason_text, password, kind) = match (direct, mediated) {
        (Some(invite), None) => (
            entity(invite.attr("jid")?, true)?,
            entity(message.attr("from")?, false)?,
            invite.attr("reason").unwrap_or("").to_owned(),
            invite.attr("password").map(str::to_owned),
            Kind::Direct,
        ),
        (compatibility, Some(extension)) => {
            let invite = unique(extension, "invite", USER)??;
            if unique(extension, "decline", USER)?.is_some() {
                return None;
            }
            let reason = unique(invite, "reason", USER)?;
            let password = unique(extension, "password", USER)?;
            if reason.is_some_and(|value| value.children().next().is_some())
                || password.is_some_and(|value| value.children().next().is_some())
            {
                return None;
            }
            let room = entity(message.attr("from")?, true)?;
            let password = password.map(Element::text);
            // XEP-0045 documents this legacy tag, still emitted by servers such as Prosody.
            // It is a duplicate transport hint, never a second invitation or password source.
            if let Some(compatibility) = compatibility {
                if entity(compatibility.attr("jid")?, true)? != room
                    || compatibility
                        .attr("password")
                        .filter(|value| !value.is_empty())
                        .is_some_and(|value| password.as_deref() != Some(value))
                {
                    return None;
                }
            }
            (
                room,
                entity(invite.attr("from")?, false)?,
                reason.map(Element::text).unwrap_or_default(),
                password,
                Kind::Mediated,
            )
        }
        _ => return None,
    };
    if inviter.parse::<Jid>().ok()?.to_bare().as_str() == owner
        || password
            .as_ref()
            .is_some_and(|password| password.encode_utf16().count() > 4096)
    {
        return None;
    }
    Some(Incoming {
        room,
        inviter,
        reason: reason(&reason_text),
        kind,
        password: password
            .filter(|value| !value.is_empty())
            .map(|value| Password(zeroize::Zeroizing::new(value))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_live_invites_and_rejects_replays_ambiguity_and_wrong_accounts() {
        let owner = "me@example.org";
        let direct = "<message xmlns='jabber:client' from='friend@example.org/phone' to='me@example.org/desktop'><x xmlns='jabber:x:conference' jid='room@conference.example.org' reason='Join us' password='private-password'/></message>";
        let xml: Element = direct.parse().unwrap();
        let invite = parse(&xml, owner).unwrap();
        assert_eq!(invite.kind, Kind::Direct);
        assert_eq!(invite.room, "room@conference.example.org");
        assert_eq!(
            invite.password.as_ref().unwrap().0.as_str(),
            "private-password"
        );
        assert!(!format!("{invite:?}").contains("private-password"));
        for invalid in [
            direct.replace("to='me@example.org/desktop'", "to='other@example.org'"),
            direct.replace("friend@example.org/phone", "me@example.org/phone"),
            direct.replace("friend@example.org/phone", "example.org"),
            direct.replace(
                "room@conference.example.org",
                "room@conference.example.org/nick",
            ),
            direct.replace("<x xmlns=", "<x xmlns='jabber:x:conference'/><x xmlns="),
            direct.replace("<message xmlns=", "<message type='groupchat' xmlns="),
            direct.replace("</message>", "<delay xmlns='urn:xmpp:delay'/></message>"),
            direct.replace(
                "</message>",
                "<forwarded xmlns='urn:xmpp:forward:0'/></message>",
            ),
            direct.replace(
                "</message>",
                "<received xmlns='urn:xmpp:carbons:2'/></message>",
            ),
            direct.replace("</message>", "<result xmlns='urn:xmpp:mam:2'/></message>"),
        ] {
            let invalid: Element = invalid.parse().unwrap();
            assert!(recognized(&invalid));
            assert!(parse(&invalid, owner).is_none());
        }
        let mediated: Element = "<message xmlns='jabber:client' from='room@conference.example.org'><x xmlns='http://jabber.org/protocol/muc#user'><invite from='friend@example.org'><reason>See you there</reason></invite><password>secret</password></x></message>".parse().unwrap();
        assert_eq!(parse(&mediated, owner).unwrap().kind, Kind::Mediated);
        let mut compatible = mediated.clone();
        compatible.append_child(
            Element::builder("x", DIRECT)
                .attr("jid".try_into().unwrap(), "room@conference.example.org")
                .attr("password".try_into().unwrap(), "secret")
                .build(),
        );
        let legacy = parse(&compatible, owner).unwrap();
        assert_eq!(legacy.kind, Kind::Mediated);
        assert_eq!(legacy.inviter, "friend@example.org");
        for (attribute, value) in [
            ("jid", "other@conference.example.org"),
            ("password", "conflicting-secret"),
        ] {
            let mut conflicting = compatible.clone();
            conflicting
                .children_mut()
                .find(|child| child.is("x", DIRECT))
                .unwrap()
                .set_attr("".into(), attribute.try_into().unwrap(), value);
            assert!(parse(&conflicting, owner).is_none());
        }
        let mut long_password = xml.clone();
        long_password.children_mut().next().unwrap().set_attr(
            "".into(),
            "password".try_into().unwrap(),
            "🦊".repeat(2048),
        );
        assert!(parse(&long_password, owner).is_some());
        let mut too_long = xml.clone();
        too_long.children_mut().next().unwrap().set_attr(
            "".into(),
            "password".try_into().unwrap(),
            "🦊".repeat(2049),
        );
        assert!(parse(&too_long, owner).is_none());
        let mut bounded = xml;
        bounded.children_mut().next().unwrap().set_attr(
            "".into(),
            "reason".try_into().unwrap(),
            "🦊".repeat(257),
        );
        assert_eq!(
            parse(&bounded, owner)
                .unwrap()
                .reason
                .encode_utf16()
                .count(),
            512
        );
    }
}
