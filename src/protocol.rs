//! Standards-only XMPP boundary. No GTK or implicit plaintext fallback.
use crate::crypto::{legacy, service as modern, pq::service as pq};
use crate::encryption_policy::Mode;
use crate::model::{Account, Message};
use crate::rooms::archive;
use crate::transport::{Client, Event};
use chrono::{DateTime, Utc};
use std::{collections::HashMap, sync::mpsc, thread, time::Duration};
use tokio::sync::mpsc as channel;
use tokio_xmpp::{jid::BareJid, minidom::Element, parsers::stanza::Stanza};
const NS: &str = "jabber:client";
const SID: &str = "urn:xmpp:sid:0";
const REPLY: &str = "urn:xmpp:reply:0";
const FALLBACK: &str = "urn:xmpp:fallback:0";
pub fn bare(value: &str) -> Result<String, String> {
    let jid = value
        .trim()
        .parse::<BareJid>()
        .map_err(|_| "Enter an XMPP address without a resource".to_string())?;
    if jid.node().is_none() {
        return Err("An account name and domain are required".into());
    }
    Ok(jid.to_string())
}
fn bare_of(value: &str) -> String {
    value.split('/').next().unwrap_or("").to_string()
}
fn subscription_peer(presence: &Element, owner: &str) -> Option<String> {
    if !presence.is("presence", NS)
        || !matches!(
            presence.attr("type"),
            Some("subscribe" | "unsubscribe" | "subscribed" | "unsubscribed")
        )
        || presence.attr("to").is_some_and(|to| {
            to.parse::<tokio_xmpp::jid::Jid>()
                .ok()
                .is_none_or(|jid| jid.to_bare().as_str() != owner)
        })
    {
        return None;
    }
    if presence.children().any(|child| {
        matches!(
            child.ns().as_str(),
            "urn:xmpp:delay"
                | "jabber:x:delay"
                | "urn:xmpp:carbons:2"
                | "urn:xmpp:forward:0"
                | "urn:xmpp:mam:1"
                | "urn:xmpp:mam:2"
        )
    }) {
        return None;
    }
    let from = presence.attr("from")?;
    (from != owner && bare(from).ok().as_deref() == Some(from)).then(|| from.to_owned())
}
pub fn el(name: &str, ns: &str) -> Element {
    Element::builder(name, ns).build()
}
fn carbon_payload(wrapper: &Element) -> Option<(&Element, bool)> {
    let forwarded = wrapper.get_child("forwarded", "urn:xmpp:forward:0")?;
    let message = forwarded.get_child("message", NS)?;
    let historical = forwarded.get_child("delay", "urn:xmpp:delay").is_some()
        || forwarded.get_child("x", "jabber:x:delay").is_some();
    Some((message, historical))
}
pub fn reply_content(xml: &Element, body: &str) -> (String, String, String, String) {
    reply_projection(xml, body).0
}
pub fn reply_projection(
    xml: &Element,
    body: &str,
) -> ((String, String, String, String), Vec<(usize, usize)>) {
    let replies: Vec<_> = xml.children().filter(|c| c.is("reply", REPLY)).collect();
    if replies.len() != 1 {
        return (empty_reply(body), Vec::new());
    }
    let r = replies[0];
    let id = r.attr("id").unwrap_or("");
    if !crate::message_actions::valid_id(id) {
        return (empty_reply(body), Vec::new());
    }
    let author = bare_of(r.attr("to").unwrap_or(""));
    if !author.is_empty() && bare(&author).is_err() {
        return (empty_reply(body), Vec::new());
    }
    let chars: Vec<char> = body.chars().collect();
    let mut ranges = vec![];
    let parsed = (|| -> Option<()> {
        for f in xml
            .children()
            .filter(|c| c.is("fallback", FALLBACK) && c.attr("for") == Some(REPLY))
        {
            if f.children().next().is_none() {
                ranges.push((0, chars.len()))
            }
            for p in f.children().filter(|p| p.is("body", FALLBACK)) {
                if p.attr("xml:lang").is_some() {
                    continue;
                }
                let (a, b) = match (p.attr("start"), p.attr("end")) {
                    (None, None) => (0, chars.len()),
                    (Some(a), Some(b)) => (a.parse().ok()?, b.parse().ok()?),
                    _ => return None,
                };
                if a > b || b > chars.len() {
                    return None;
                }
                ranges.push((a, b));
                if ranges.len() > 64 {
                    return None;
                }
            }
        }
        Some(())
    })();
    if parsed.is_none() {
        return ((id.into(), author, String::new(), body.into()), Vec::new());
    }
    ranges.sort();
    let removed = ranges.clone();
    let mut pos = 0;
    let mut quote = String::new();
    let mut clean = String::new();
    for (a, b) in ranges {
        if a > pos {
            clean.extend(&chars[pos..a])
        }
        if b > a.max(pos) {
            quote.extend(&chars[a.max(pos)..b])
        }
        pos = pos.max(b)
    }
    clean.extend(&chars[pos..]);
    ((id.into(), author, quote.trim().into(), clean), removed)
}
fn empty_reply(body: &str) -> (String, String, String, String) {
    (String::new(), String::new(), String::new(), body.into())
}
pub fn normalize(
    xml: &Element,
    account: &str,
    archive: Option<(&str, &str)>,
) -> Option<(String, Message, bool)> {
    normalize_content(xml, account, archive, false)
}
pub fn normalize_protected(
    xml: &Element,
    account: &str,
    archive: Option<(&str, &str)>,
) -> Option<(String, Message, bool)> {
    normalize_content(xml, account, archive, true)
}
fn normalize_content(
    xml: &Element,
    account: &str,
    archive: Option<(&str, &str)>,
    encrypted: bool,
) -> Option<(String, Message, bool)> {
    if !xml.is("message", NS) || !matches!(xml.attr("type").unwrap_or("normal"), "chat" | "normal")
    {
        return None;
    }
    if xml
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .is_some()
        || xml
            .get_child("x", crate::rooms::invitations::DIRECT)
            .is_some()
    {
        return None;
    }
    let from = bare_of(xml.attr("from")?);
    let to = bare_of(xml.attr("to")?);
    let own = from == account;
    let peer = if own { to.clone() } else { from.clone() };
    if bare(&peer).is_err() || (!own && to != account) {
        return None;
    }
    let unsupported_pq = xml.get_child("encrypted", "urn:monocles:omemo-pq:1").is_some();
    let unavailable = unsupported_pq || xml.children().any(|c| {
        c.is("encrypted", "urn:xmpp:omemo:2")
            || c.is("encrypted", "eu.siacs.conversations.axolotl")
            || c.is("encryption", "urn:xmpp:eme:0")
    });
    let action = if unavailable {
        None
    } else {
        crate::message_actions::parse(xml).ok()?
    };
    let control = matches!(
        action,
        Some(
            crate::message_actions::Action::React { .. }
                | crate::message_actions::Action::Retract { .. }
        )
    );
    let poll_present = !unavailable && xml.get_child("poll", crate::polls::NS).is_some();
    if poll_present && control {
        return None;
    }
    let quoted = if unavailable || control {
        None
    } else {
        crate::forwarding::standalone(xml)
    };
    let attachment = if unavailable || control || poll_present {
        None
    } else {
        crate::file_share::Attachment::incoming(
            xml,
            &xml.get_child("body", NS)
                .map(|b| b.text())
                .unwrap_or_default(),
            encrypted,
        )
    };
    let poll = if unavailable {
        None
    } else {
        crate::polls::Poll::parse(xml).ok().flatten()
    };
    let raw = if unsupported_pq {
        "Encrypted message — waiting for OMEMO2 decryption.".into()
    } else if unavailable {
        "Encrypted message — this encryption format is unavailable or needs device review.".into()
    } else if control {
        String::new()
    } else if let Some(body) = xml.get_child("body", NS) {
        body.text()
    } else if poll.is_some() {
        String::new()
    } else if poll_present {
        "Unsupported poll".into()
    } else if quoted.is_some() || attachment.is_some() {
        String::new()
    } else {
        return None;
    };
    let (reply_id, reply_to, quote, text) = if unavailable || control {
        empty_reply(&raw)
    } else {
        reply_content(xml, &raw)
    };
    let text = if let Some(file) = &attachment {
        file.caption(xml, &raw)
    } else if poll.is_some() {
        let namespaces = if reply_id.is_empty() {
            vec![crate::polls::NS]
        } else {
            vec![crate::polls::NS, REPLY]
        };
        crate::polls::Poll::body_without_fallbacks(xml, &namespaces)
    } else {
        text
    };
    let mut m = Message {
        attachment,
        unsupported_attachment: xml.get_child("file-sharing", "urn:xmpp:sfs:0").is_some()
            || xml.get_child("x", "jabber:x:oob").is_some(),
        poll_source: if poll_present {
            crate::polls::Source::Structured
        } else {
            crate::polls::Source::None
        },
        poll_fallback: poll
            .as_ref()
            .is_some_and(|p| p.accompanying_body(xml) != raw),
        kind: if poll.is_some() { "poll" } else { "text" }.into(),
        poll,
        action,
        sender: xml.attr("from").unwrap_or("").into(),
        own,
        text,
        encrypted: encrypted && !unavailable,
        unavailable,
        delivery: if own { "sent" } else { "received" }.into(),
        reply_id,
        reply_to,
        reply: quote,
        ..Default::default()
    };
    if m.attachment.is_some() {
        m.kind = "file".into();
        m.unsupported_attachment = true;
    }
    if crate::forwarding::parse(&m.text).is_none()
        && let Some((attribution, body)) = quoted
    {
        m.text = if m.text.is_empty() {
            body
        } else {
            format!("{}\n\n{body}", m.text)
        };
        m.forwarded = Some(attribution);
    }
    if xml.get_child("file-sharing", "urn:xmpp:sfs:0").is_some() && !poll_present {
        m.poll_source = crate::polls::Source::Text;
    }
    m.hydrate_compatibility();
    m.markable_id = if !own && m.action.is_none() {
        crate::read_markers::markable_id(xml).unwrap_or_default()
    } else {
        String::new()
    };
    m.inline_emoji = (!unavailable && m.attachment.is_none() && !control)
        .then(|| crate::packs::normalized_inline(xml, &m.text))
        .flatten();
    m.preview_spoilers = if unavailable {
        None
    } else {
        crate::spoilers::normalized(xml, &m.text)
    };
    m.wire_id = xml
        .attr("id")
        .filter(|id| crate::message_actions::valid_id(id))
        .unwrap_or("")
        .into();
    if !m.wire_id.is_empty() {
        m.aliases.push(format!("wire:{}", m.wire_id))
    }
    for child in xml.children() {
        if child.is("origin-id", SID) {
            if let Some(id) = child
                .attr("id")
                .filter(|id| crate::message_actions::valid_id(id))
            {
                m.aliases.push(format!("origin:{id}"))
            }
        } else if child.is("stanza-id", SID) && child.attr("by") == Some(account) {
            if let Some(id) = child
                .attr("id")
                .filter(|id| crate::message_actions::valid_id(id))
            {
                m.aliases.push(format!("server:{id}"))
            }
        }
    }
    let stamp = archive.map(|(_, s)| s.to_string()).or_else(|| {
        xml.get_child("delay", "urn:xmpp:delay")
            .and_then(|d| d.attr("stamp"))
            .map(str::to_owned)
    });
    if let Some(ref stamp) = stamp {
        if let Ok(time) = DateTime::parse_from_rfc3339(stamp) {
            m.timestamp = time
                .with_timezone(&Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
            m.time = time
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        }
    }
    if let Some((id, _)) = archive.filter(|(id, _)| crate::message_actions::valid_id(id)) {
        m.aliases.push(format!("archive:{id}"))
    }
    if m.action.is_some() && m.aliases.is_empty() {
        let mut raw = Vec::new();
        xml.write_to(&mut raw).ok()?;
        raw.extend(m.timestamp.as_bytes());
        m.aliases.push(format!(
            "control:{}",
            crate::crypto::lifecycle::hex(&openssl::sha::sha256(&raw))
        ));
    }
    Some((peer, m, archive.is_some() || stamp.is_some()))
}
pub fn text_stanza(peer: &str, text: &str, id: &str, reply: Option<(&str, &str, &str)>) -> Element {
    let mut root = Element::builder("message", NS)
        .attr("to".try_into().expect("static XML attribute"), peer)
        .attr("type".try_into().expect("static XML attribute"), "chat")
        .attr("id".try_into().expect("static XML attribute"), id)
        .build();
    let mut body = text.to_string();
    if let Some((reference, author, quote)) = reply {
        if !reference.is_empty() {
            root.append_child(
                Element::builder("reply", REPLY)
                    .attr("id".try_into().expect("static XML attribute"), reference)
                    .attr("to".try_into().expect("static XML attribute"), author)
                    .build(),
            );
            if !quote.is_empty() {
                let quoted: String = quote.chars().take(512).collect();
                let prefix = format!(
                    "> {}{}\n",
                    quoted.replace('\n', "\n> "),
                    if quote.chars().count() > 512 {
                        "…"
                    } else {
                        ""
                    }
                );
                let end = prefix.chars().count().to_string();
                body = prefix + text;
                root.append_child(
                    Element::builder("fallback", FALLBACK)
                        .attr("for".try_into().expect("static XML attribute"), REPLY)
                        .append(
                            Element::builder("body", FALLBACK)
                                .attr("start".try_into().expect("static XML attribute"), "0")
                                .attr("end".try_into().expect("static XML attribute"), end)
                                .build(),
                        )
                        .build(),
                );
            }
        }
    }
    root.append_child(Element::builder("body", NS).append(body).build());
    root.append_child(
        Element::builder("origin-id", SID)
            .attr("id".try_into().expect("static XML attribute"), id)
            .build(),
    );
    root.append_child(el("request", "urn:xmpp:receipts"));
    root
}
pub fn outgoing(peer: &str, message: &Message, quote: &str) -> Element {
    let mut root = outgoing_content(peer, message, quote);
    crate::read_markers::decorate(&mut root, message);
    root
}
fn outgoing_content(peer: &str, message: &Message, quote: &str) -> Element {
    let accompanied = message
        .forwarded
        .as_ref()
        .map(|a| a.wire_body(&message.text))
        .unwrap_or_else(|| message.text.clone());
    if let Some(file) = &message.attachment {
        let body = if accompanied.is_empty() {
            file.wire_url.clone()
        } else {
            format!("{accompanied}\n{}", file.wire_url)
        };
        let mut root = text_stanza(
            peer,
            &body,
            &message.wire_id,
            (!message.reply_id.is_empty()).then_some((&message.reply_id, &message.reply_to, quote)),
        );
        file.add_to(&mut root);
        root.append_child(el("store", "urn:xmpp:hints"));
        return root;
    }
    if message.poll_source() == crate::polls::Source::Legacy {
        let mut root = text_stanza(
            peer,
            &accompanied,
            &message.wire_id,
            (!message.reply_id.is_empty()).then_some((&message.reply_id, &message.reply_to, quote)),
        );
        if let Some(action) = &message.action {
            crate::message_actions::decorate(&mut root, action);
        }
        root.append_child(el("store", "urn:xmpp:hints"));
        return root;
    }
    let poll_body = message
        .poll
        .as_ref()
        .filter(|_| message.poll_fallback)
        .map(|p| p.readable_body());
    let authored = if let Some(fallback) = &poll_body {
        if accompanied.is_empty() {
            fallback.clone()
        } else {
            format!("{accompanied}\n\n{fallback}")
        }
    } else {
        accompanied.clone()
    };
    let body = if matches!(
        message.action,
        Some(crate::message_actions::Action::Retract { .. })
    ) {
        "A previous message was retracted."
    } else {
        &authored
    };
    let mut root = text_stanza(
        peer,
        body,
        &message.wire_id,
        (!message.reply_id.is_empty()).then_some((&message.reply_id, &message.reply_to, quote)),
    );
    if let Some(poll) = &message.poll {
        root.append_child(poll.element());
        if poll_body.is_some() {
            let wire_body = root.get_child("body", NS).unwrap().text();
            root.append_child(
                Element::builder("fallback", FALLBACK)
                    .attr("for".try_into().unwrap(), crate::polls::NS)
                    .append(
                        Element::builder("body", FALLBACK)
                            .attr(
                                "start".try_into().unwrap(),
                                (wire_body.chars().count()
                                    - poll_body.as_ref().unwrap().chars().count()
                                    - if accompanied.is_empty() { 0 } else { 2 })
                                .to_string(),
                            )
                            .attr(
                                "end".try_into().unwrap(),
                                wire_body.chars().count().to_string(),
                            )
                            .build(),
                    )
                    .build(),
            );
        } else if body.is_empty() && message.reply_id.is_empty() {
            let nodes = root.take_nodes();
            for node in nodes {
                if node.as_element().is_none_or(|e| !e.is("body", NS)) {
                    root.append_node(node);
                }
            }
        }
        if root.get_child("store", "urn:xmpp:hints").is_none() {
            root.append_child(el("store", "urn:xmpp:hints"));
        }
    }
    if let Some(action) = &message.action {
        crate::message_actions::decorate(&mut root, action);
    }
    crate::packs::decorate_inline(&mut root, message);
    root
}
#[derive(Debug, Clone)]
pub enum BackendEvent {
    Read {
        peer: String,
        id: String,
    },
    OwnRead {
        peer: String,
        id: String,
    },
    InlineEmoji {
        peer: String,
        id: String,
        emoji: crate::packs::InlineEmoji,
    },
    Uncertain {
        peer: String,
        id: String,
    },
    CryptoStatus(crate::crypto::service::Status),
    PqCrypto(crate::crypto::service::Status),
    PqDevices {
        peer: String,
        devices: Vec<crate::crypto::pq::service::Review>,
    },
    PqEncrypted {
        peer: String,
        message: Message,
        historical: bool,
        receipt_to: Option<String>,
        token: crate::crypto::pq::wire::Token,
    },
    CryptoReview {
        peer: String,
        rows: Vec<crate::crypto::service::Review>,
        errors: Vec<String>,
    },
    CryptoError {
        peer: String,
        text: String,
    },
    PreviewSpoilers {
        peer: String,
        id: String,
        context: crate::spoilers::Context,
    },
    CryptoAliases {
        markable_id: String,
        legacy_sticker: bool,
        legacy_reply: Option<crate::crypto::legacy::service::LegacyReply>,
        peer: String,
        own: bool,
        id: String,
        aliases: Vec<String>,
    },
    EncryptedMessage {
        peer: String,
        message: Message,
        historical: bool,
        receipt_to: Option<String>,
        token: crate::crypto::journal::Token,
    },
    LegacyStatus(crate::crypto::service::Status),
    LegacyReview {
        peer: String,
        rows: Vec<crate::crypto::service::Review>,
        errors: Vec<String>,
    },
    LegacyEncryptedMessage {
        peer: String,
        message: Message,
        historical: bool,
        receipt_to: Option<String>,
        token: crate::crypto::journal::Token,
    },
    Status {
        online: bool,
        error: bool,
        text: String,
    },
    Roster(crate::contact_access::RosterUpdate),
    ContactAccessError(String),
    RoomBookmarks(Vec<crate::rooms::BookmarkInfo>),
    RoomPresence(crate::rooms::Presence),
    RoomError(String),
    Presence {
        peer: String,
        online: bool,
        visible: bool,
    },
    AvatarChanged {
        peer: String,
        hint: Option<String>,
    },
    Typing {
        peer: String,
        composing: bool,
    },
    Message {
        peer: String,
        message: Message,
        historical: bool,
        receipt_to: Option<String>,
    },
    Sent {
        peer: String,
        id: String,
    },
    Receipt {
        peer: String,
        id: String,
    },
    Failed {
        peer: String,
        id: String,
    },
    History {
        peer: String,
        before: String,
        complete: bool,
        error: bool,
    },
    Request(String),
    RequestCancelled(String),
    RoomInvitation(crate::rooms::invitations::Incoming),
}
#[derive(Debug)]
pub enum RoomPreparation {
    Send {
        message: Message,
        quote: String,
    },
    Probe {
        reply: tokio::sync::oneshot::Sender<crate::crypto::Result<Mode>>,
    },
    Review,
    PqReview,
}
#[derive(Debug)]
pub enum Command {
    CryptoReady(bool),
    LegacyReady(bool),
    PqReady(bool),
    ChooseEncryption {
        peer: String,
        reply: tokio::sync::oneshot::Sender<crate::crypto::Result<crate::encryption_policy::Mode>>,
    },
    Crypto(crate::crypto::service::Request),
    Legacy(crate::crypto::legacy::service::Request),
    PqCrypto(crate::crypto::pq::service::Request),
    LoadRoomBookmarks,
    UpdateRoomChoices(Vec<crate::rooms::LocalBookmark>),
    RoomServices {
        reply: tokio::sync::oneshot::Sender<Result<Vec<String>, crate::rooms::Error>>,
    },
    CreateRoom {
        service: String,
        title: String,
        nick: String,
        reply:
            tokio::sync::oneshot::Sender<Result<crate::rooms::BookmarkInfo, crate::rooms::Error>>,
    },
    JoinExistingRoom {
        peer: String,
        nick: String,
        password: Option<crate::rooms::Password>,
        reply:
            tokio::sync::oneshot::Sender<Result<crate::rooms::BookmarkInfo, crate::rooms::Error>>,
    },
    LeaveRoom {
        peer: String,
        reply: tokio::sync::oneshot::Sender<Result<(), crate::rooms::Error>>,
    },
    InviteRoom {
        peer: String,
        members: Vec<String>,
        reply: tokio::sync::oneshot::Sender<Result<Vec<String>, crate::rooms::Error>>,
    },
    ManagedJoin {
        epoch: u64,
        peer: String,
        nick: String,
        password: Option<crate::rooms::Password>,
        history: u32,
        ticket: crate::rooms::manage::Ticket,
        reply: tokio::sync::oneshot::Sender<
            Result<crate::rooms::manage::JoinReceipt, crate::rooms::Error>,
        >,
    },
    ManagedUnlock {
        epoch: u64,
        peer: String,
        id: u64,
        ticket: crate::rooms::manage::Ticket,
        reply: tokio::sync::oneshot::Sender<Result<(), crate::rooms::Error>>,
    },
    ManagedLeave {
        epoch: u64,
        peer: String,
        id: u64,
        ticket: crate::rooms::manage::Ticket,
        reply: tokio::sync::oneshot::Sender<Result<(), crate::rooms::Error>>,
    },
    ManagedRoomStanza {
        epoch: u64,
        peer: String,
        id: u64,
        ticket: crate::rooms::manage::Ticket,
        xml: Element,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    ManagedIq {
        epoch: u64,
        peer: String,
        id: u64,
        ticket: crate::rooms::manage::Ticket,
        kind: crate::iq::Kind,
        payload: crate::rooms::manage::Form,
        reply: tokio::sync::oneshot::Sender<crate::iq::Reply>,
    },
    RoomOperationFinished {
        peer: String,
        ticket: crate::rooms::manage::Ticket,
    },
    ManagedCancel {
        epoch: u64,
        peer: String,
        ticket: crate::rooms::manage::Ticket,
        leave: bool,
    },
    RoomBookmarksLoaded {
        epoch: u64,
        result: Result<Vec<crate::rooms::Bookmark>, crate::rooms::Error>,
    },
    JoinRoom {
        peer: String,
        nick: Option<String>,
    },
    RoomStableIds {
        epoch: u64,
        peer: String,
    },
    RoomMembersLoaded {
        epoch: u64,
        peer: String,
        revision: u64,
        result: Result<crate::rooms::members::MemberSnapshot, crate::rooms::Error>,
    },
    RoomPrepared {
        epoch: u64,
        peer: String,
        revision: u64,
        result: Result<crate::rooms::members::MemberSnapshot, crate::rooms::Error>,
        purpose: RoomPreparation,
    },
    ReviewRoom {
        peer: String,
    },
    RoomWire {
        xml: Element,
        peer: String,
        epoch: u64,
        revision: u64,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    Avatar {
        peer: String,
        source: crate::avatar::Source,
        reply: tokio::sync::oneshot::Sender<
            Result<Option<crate::avatar::Avatar>, crate::avatar::Error>,
        >,
    },
    Wire {
        xml: Element,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    Iq {
        to: String,
        kind: crate::iq::Kind,
        payload: Element,
        reply: tokio::sync::oneshot::Sender<crate::iq::Reply>,
    },
    Send {
        peer: String,
        message: Message,
        quote: String,
    },
    History {
        peer: String,
        before: String,
    },
    PreviewHistory {
        peer: String,
        start: String,
        end: String,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    PreviewHistoryNext {
        job: PreviewHistoryJob,
        before: String,
        complete: bool,
    },
    Typing {
        peer: String,
        composing: bool,
    },
    ContactAccessState {
        peer: String,
        reply: tokio::sync::oneshot::Sender<Result<crate::contact_access::State, String>>,
    },
    ContactAccess {
        peer: String,
        action: crate::contact_access::Action,
        reply: tokio::sync::oneshot::Sender<Result<crate::contact_access::State, String>>,
    },
    ContactIq {
        epoch: u64,
        kind: crate::iq::Kind,
        payload: Element,
        reply: tokio::sync::oneshot::Sender<crate::iq::Reply>,
    },
    ContactWire {
        epoch: u64,
        peer: String,
        kind: String,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    Authorize {
        peer: String,
        allow: bool,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Displayed {
        peer: String,
        id: String,
    },
    DisplayedReady {
        candidate: crate::read_markers::Candidate,
        epoch: u64,
    },
    DisplayedPolicy {
        enabled: bool,
        revision: u64,
    },
    FlushDisplayed,
    Receipt {
        peer: String,
        id: String,
    },
}
#[derive(Debug)]
pub(crate) struct PreviewHistoryJob {
    peer: String,
    start: String,
    end: String,
    epoch: u64,
    token: String,
    cancel: tokio::sync::watch::Receiver<u64>,
    deadline: std::time::Instant,
    reply: tokio::sync::oneshot::Sender<bool>,
    pending: Vec<tokio::sync::oneshot::Receiver<bool>>,
    cursors: std::collections::HashSet<String>,
    results: std::collections::HashSet<String>,
    page_count: usize,
    failed: bool,
}
async fn preview_history_page(
    network: &channel::UnboundedSender<Command>,
    mut job: PreviewHistoryJob,
    before: String,
    complete: bool,
) {
    let pending = std::mem::take(&mut job.pending);
    let remaining = job
        .deadline
        .saturating_duration_since(std::time::Instant::now());
    tokio::select! {
        _ = job.reply.closed() => job.failed = true,
        _ = job.cancel.changed() => job.failed = true,
        result = tokio::time::timeout(remaining, async move {
            for reply in pending { if !reply.await.unwrap_or(false) { return false; } } true
        }) => job.failed |= !matches!(result, Ok(true)),
    }
    let _ = network.send(Command::PreviewHistoryNext {
        job,
        before,
        complete,
    });
}
fn finish_preview_history(
    active: &mut HashMap<String, String>,
    job: PreviewHistoryJob,
    success: bool,
) {
    if active.get(&job.peer) == Some(&job.token) {
        active.remove(&job.peer);
    }
    let _ = job.reply.send(success);
}
pub type Envelope = (u64, String, BackendEvent);
async fn room_operation<T>(
    network: &channel::UnboundedSender<Command>,
    peer: &str,
    epoch: u64,
    ticket: &crate::rooms::manage::Ticket,
    mut reply: tokio::sync::oneshot::Sender<Result<T, crate::rooms::Error>>,
    leave: bool,
    work: impl std::future::Future<Output = Result<T, crate::rooms::Error>>,
) {
    let result = tokio::select! {value=work=>Some(value),_=reply.closed()=>None};
    let abandoned = match result {
        Some(value) => reply.send(value).is_err(),
        None => true,
    };
    if abandoned {
        let _ = network.send(Command::ManagedCancel {
            epoch,
            peer: peer.into(),
            ticket: ticket.clone(),
            leave,
        });
    }
    let _ = network.send(Command::RoomOperationFinished {
        peer: peer.into(),
        ticket: ticket.clone(),
    });
}
fn prepare_room(
    rooms: &crate::rooms::Rooms,
    peer: String,
    epoch: u64,
    purpose: RoomPreparation,
    network: &channel::UnboundedSender<Command>,
) {
    let revision = rooms.recipient_revision(&peer);
    let tx = network.clone();
    tokio::spawn(async move {
        let (revision, result) = match revision {
            Ok(revision) => (
                revision,
                crate::rooms::members::fetch(&crate::iq::Client::new(tx.clone()), &peer).await,
            ),
            Err(error) => (0, Err(error)),
        };
        let _ = tx.send(Command::RoomPrepared {
            epoch,
            peer,
            revision,
            result,
            purpose,
        });
    });
}
fn refresh_room_members(
    rooms: &mut crate::rooms::Rooms,
    peer: &str,
    epoch: u64,
    network: &channel::UnboundedSender<Command>,
) {
    let Some(revision) = rooms.begin_member_refresh(peer) else {
        return;
    };
    let peer = peer.to_owned();
    let tx = network.clone();
    tokio::spawn(async move {
        let iq = crate::iq::Client::new(tx.clone());
        let result = crate::rooms::members::fetch(&iq, &peer).await;
        let _ = tx.send(Command::RoomMembersLoaded {
            epoch,
            peer,
            revision,
            result,
        });
    });
}

pub struct Session {
    pub tx: channel::UnboundedSender<Command>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Session {
    pub fn start(
        config: Account,
        password: String,
        generation: u64,
        events: mpsc::Sender<Envelope>,
    ) -> Self {
        Self::start_with_rooms(config, password, generation, events, vec![])
    }
    pub fn start_with_rooms(
        config: Account,
        password: String,
        generation: u64,
        events: mpsc::Sender<Envelope>,
        choices: Vec<crate::rooms::LocalBookmark>,
    ) -> Self {
        let (tx, mut rx) = channel::unbounded_channel();
        let (cancel, stopped) = tokio::sync::oneshot::channel();
        let network = tx.clone();
        thread::Builder::new().name("monogram-xmpp".into()).spawn(move||{
            let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().expect("network runtime");
            runtime.block_on(async move{ tokio::select! { _ = stopped => {}, _ = async move {
                let owner=config.jid.clone();let emit=|e|{let _=events.send((generation,owner.clone(),e));};
                let Ok(_jid)=bare(&config.jid)else{emit(BackendEvent::Status{online:false,error:true,text:"Invalid XMPP address".into()});return};
                let mut client = Client::new(config, password);
                let crypto=modern::Handle::start(owner.clone(),generation,network.clone(),events.clone());
                let legacy=legacy::service::Handle::start(owner.clone(),generation,network.clone(),events.clone());
                let pq=pq::Handle::start(owner.clone(),generation,network.clone(),events.clone());
                let contacts=crate::contact_access::Manager::new(owner.clone(),generation,events.clone()).await;
                let mut contact_queries=HashMap::<String,(u64,std::time::Instant,u64)>::new();
                let mut displayed_policy=crate::read_markers::load_policy(&owner).await;
                let marker_flush=std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let mut marker_tick=std::time::Instant::now();
                let mut preview_jobs=HashMap::<String,PreviewHistoryJob>::new();
                let mut preview_active=HashMap::<String,String>::new();
                let (preview_epoch,_) = tokio::sync::watch::channel(0u64);
                let mut iq=crate::iq::Pending::default();
                let mut rooms=crate::rooms::Rooms::new(owner.clone());
                rooms.update_choices(choices);
                let mut room_jobs=crate::rooms::manage::Pending::default();
                let mut room_epoch=0u64;
                let mut loading_rooms=false;
                let mut subscribing_rooms=false;
                let mut crypto_ready=false;let mut legacy_ready=false;let mut pq_ready=false;let mut online=false;let mut mam:HashMap<String,archive::Page>=HashMap::new();let mut timer=tokio::time::interval(Duration::from_secs(1));let mut handshake=std::time::Instant::now();
                emit(BackendEvent::Status{online:false,error:false,text:"Connecting…".into()});
                loop{tokio::select!{
                    command=rx.recv()=>{match command{
                        None=>break,
                        Some(Command::CryptoReady(ready))=>crypto_ready=ready,
                        Some(Command::LegacyReady(ready))=>legacy_ready=ready,
                        Some(Command::PqReady(ready))=>pq_ready=ready,
                        Some(Command::Crypto(request))=>crypto.send(request),
                        Some(Command::Legacy(request))=>legacy.send(request),
                        Some(Command::PqCrypto(pq::Request::Review(peer))) if rooms.known(&peer)=>prepare_room(&rooms,peer,room_epoch,RoomPreparation::PqReview,&network),
                        Some(Command::PqCrypto(request))=>pq.send(request),
                        Some(Command::UpdateRoomChoices(choices))=>{
                            rooms.update_choices(choices);
                            if online {for peer in rooms.autojoin(){let _=network.send(Command::JoinRoom{peer,nick:None});}}
                        },
                        Some(Command::RoomServices{reply})=>{
                            if !online {let _=reply.send(Err(crate::rooms::Error::Request(crate::iq::Error::Closed)));continue}
                            let (network,owner)=(network.clone(),owner.clone());
                            tokio::spawn(async move {let _=reply.send(crate::rooms::manage::services(&crate::iq::Client::new(network),&owner).await);});
                        },
                        Some(Command::CreateRoom{service,title,nick,reply})=>{
                            let room=match crate::rooms::manage::creation_address(&service){Ok(room)=>room,Err(error)=>{let _=reply.send(Err(error));continue}};
                            if !online {let _=reply.send(Err(crate::rooms::Error::Cancelled));continue}
                            let ticket=match room_jobs.begin(&room){Ok(ticket)=>ticket,Err(error)=>{let _=reply.send(Err(error));continue}};
                            let (network,owner,epoch)=(network.clone(),owner.clone(),room_epoch);
                            tokio::spawn(async move {
                                room_operation(&network,&room,epoch,&ticket,reply,true,crate::rooms::manage::create(&network,&owner,epoch,&room,&service,&title,&nick,&ticket)).await;
                            });
                        },
                        Some(Command::JoinExistingRoom{peer,nick,password,reply})=>{
                            if !online {let _=reply.send(Err(crate::rooms::Error::Cancelled));continue}
                            if password.is_some()&&rooms.is_joined(&peer){let _=reply.send(Err(crate::rooms::Error::AlreadyJoined));continue}
                            let ticket=match room_jobs.begin(&peer){Ok(ticket)=>ticket,Err(error)=>{let _=reply.send(Err(error));continue}};
                            let imported=rooms.imported_password(&peer);
                            let (network,owner,epoch)=(network.clone(),owner.clone(),room_epoch);
                            tokio::spawn(async move {
                                room_operation(&network,&peer,epoch,&ticket,reply,true,crate::rooms::manage::join(&network,&owner,epoch,&peer,&nick,password,imported,&ticket)).await;
                            });
                        },
                        Some(Command::InviteRoom{peer,members,reply})=>{
                            if !online||!rooms.is_joined(&peer){let _=reply.send(Err(crate::rooms::Error::NotJoined));continue}
                            let Some(id)=rooms.join_id(&peer) else{let _=reply.send(Err(crate::rooms::Error::Cancelled));continue};
                            let ticket=match room_jobs.begin(&peer){Ok(ticket)=>ticket,Err(error)=>{let _=reply.send(Err(error));continue}};
                            let (network,epoch)=(network.clone(),room_epoch);
                            tokio::spawn(async move {
                                room_operation(&network,&peer,epoch,&ticket,reply,false,crate::rooms::manage::invite(&network,&peer,members,epoch,id,&ticket)).await;
                            });
                        },
                        Some(Command::LeaveRoom{peer,reply})=>{
                            room_jobs.cancel(&peer);
                            let result=match rooms.leave(&peer){
                                Ok(stanza)=>if online&&send(&mut client,stanza).await {Ok(())}else{Err(crate::rooms::Error::Request(crate::iq::Error::Closed))},
                                Err(crate::rooms::Error::NotJoined)=>Ok(()),Err(error)=>Err(error),
                            };
                            emit(BackendEvent::RoomPresence(rooms.status(&peer)));let _=reply.send(result);
                        },
                        Some(Command::RoomOperationFinished{peer,ticket})=>room_jobs.finished(&peer,&ticket),
                        Some(Command::ManagedCancel{epoch,peer,ticket,leave})=>{
                            if epoch==room_epoch&&room_jobs.valid(&peer,&ticket){
                                let owned=room_jobs.owned_join(&peer,&ticket);room_jobs.cancel(&peer);
                                if leave&&owned.is_some()&&owned==rooms.join_id(&peer) {if let Ok(stanza)=rooms.leave(&peer){if online{send(&mut client,stanza).await;}emit(BackendEvent::RoomPresence(rooms.status(&peer)));}}
                            }
                        },
                        Some(Command::ManagedJoin{epoch,peer,nick,password,history,ticket,reply})=>{
                            if !online||epoch!=room_epoch||!room_jobs.valid(&peer,&ticket){let _=reply.send(Err(crate::rooms::Error::Cancelled));continue}
                            if room_jobs.contains(&peer){let _=reply.send(Err(crate::rooms::Error::OperationBusy));continue}
                            if let Some(receipt)=rooms.join_receipt(&peer){let _=reply.send(Ok(receipt));continue}
                            match rooms.join_with(&peer,Some(&nick),password.as_ref().map(|value|value.0.as_str()),history){
                                Ok(stanza)=>{
                                    let id=rooms.join_id(&peer).expect("new join ID");room_jobs.insert(peer.clone(),id,reply);
                                    emit(BackendEvent::RoomPresence(rooms.status(&peer)));
                                    if !send(&mut client,stanza).await{room_jobs.cancel(&peer);let _=rooms.leave(&peer);}
                                },Err(error)=>{let _=reply.send(Err(error));},
                            }
                        },
                        Some(Command::ManagedUnlock{epoch,peer,id,ticket,reply})=>{
                            let result=if online&&epoch==room_epoch&&room_jobs.valid(&peer,&ticket)&&room_jobs.owned_join(&peer,&ticket)==Some(id){rooms.configured(&peer,id)}else{Err(crate::rooms::Error::Cancelled)};
                            if result.is_ok(){emit(BackendEvent::RoomPresence(rooms.status(&peer)));refresh_room_members(&mut rooms,&peer,room_epoch,&network);}
                            let _=reply.send(result);
                        },
                        Some(Command::ManagedLeave{epoch,peer,id,ticket,reply})=>{
                            let result=if online&&epoch==room_epoch&&room_jobs.valid(&peer,&ticket)&&room_jobs.owned_join(&peer,&ticket)==Some(id)&&rooms.join_id(&peer)==Some(id){
                                room_jobs.cancel(&peer);match rooms.leave(&peer){Ok(stanza)=>{let sent=send(&mut client,stanza).await;emit(BackendEvent::RoomPresence(rooms.status(&peer)));if sent{Ok(())}else{Err(crate::rooms::Error::Request(crate::iq::Error::Closed))}},Err(error)=>Err(error)}
                            }else{Err(crate::rooms::Error::Cancelled)};
                            let _=reply.send(result);
                        },
                        Some(Command::ManagedRoomStanza{epoch,peer,id,ticket,xml,reply})=>{
                            let allowed=online&&epoch==room_epoch&&room_jobs.valid(&peer,&ticket)&&rooms.join_id(&peer)==Some(id)&&rooms.is_joined(&peer)&&xml.attr("to")==Some(peer.as_str());
                            let _=reply.send(allowed&&send(&mut client,xml).await);
                        },
                        Some(Command::ManagedIq{epoch,peer,id,ticket,kind,payload,reply})=>{
                            if !online||epoch!=room_epoch||!room_jobs.valid(&peer,&ticket)||rooms.join_id(&peer)!=Some(id)||!rooms.is_joined(&peer)||(payload.0.is("query","http://jabber.org/protocol/muc#owner")&&room_jobs.owned_join(&peer,&ticket)!=Some(id)){let _=reply.send(Err(crate::iq::Error::Closed));continue}
                            if let Some(query)=iq.start(peer,kind,payload.0,reply){let id=query.attr("id").expect("IQ ID").to_string();if !send(&mut client,query).await{iq.fail(&id);}}
                        },
                        Some(Command::LoadRoomBookmarks) if online && !loading_rooms => {
                            loading_rooms=true;
                            let tx=network.clone(); let owner=owner.clone(); let epoch=room_epoch;
                            let subscribe=!std::mem::replace(&mut subscribing_rooms,true);
                            tokio::spawn(async move {
                                let iq=crate::iq::Client::new(tx.clone());
                                let result=crate::rooms::load_bookmarks(&iq,&owner).await;
                                let _=tx.send(Command::RoomBookmarksLoaded{epoch,result});
                                if subscribe { crate::rooms::subscribe_bookmarks(&iq,&owner).await; }
                            });
                        },
                        Some(Command::LoadRoomBookmarks)=>{},
                        Some(Command::RoomBookmarksLoaded{epoch,result}) if online && epoch==room_epoch=>{
                            loading_rooms=false;
                            match result {
                                Ok(bookmarks)=>{
                                    emit(BackendEvent::RoomBookmarks(rooms.import(bookmarks)));
                                    for peer in rooms.autojoin(){ let _=network.send(Command::JoinRoom{peer,nick:None}); }
                                }
                                Err(error)=>{
                                    emit(BackendEvent::RoomError(error.to_string()));emit(BackendEvent::RoomBookmarks(rooms.cached_choices()));
                                    for peer in rooms.autojoin(){let _=network.send(Command::JoinRoom{peer,nick:None});}
                                },
                            }
                        },
                        Some(Command::RoomBookmarksLoaded{..})=>{},
                        Some(Command::RoomStableIds{epoch,peer})=>{
                            if online && epoch==room_epoch { rooms.enable_stable_ids(peer); }
                        },
                        Some(Command::RoomMembersLoaded{epoch,peer,revision,result})=>{
                            if online && epoch==room_epoch {
                                rooms.member_refresh_finished(&peer,revision,result);
                                refresh_room_members(&mut rooms,&peer,room_epoch,&network);
                            }
                        },
                        Some(Command::RoomPrepared{epoch,peer,revision,result,purpose})=>{
                            let recipients=if online && epoch==room_epoch {
                                result.and_then(|snapshot| rooms.install_recipients(&peer,revision,snapshot))
                            } else { Err(crate::rooms::Error::NotJoined) };
                            match recipients {
                                Ok(recipients)=>match purpose {
                                    RoomPreparation::Send{message,quote}=>{if message.mode()==Mode::Pq {pq.send(pq::Request::SendRoom{recipients,message,quote,epoch});}else{legacy.send(legacy::service::Request::SendRoom{recipients,message,quote,epoch});}},
                                    RoomPreparation::Probe{reply}=>{
                                        let (checked,response)=tokio::sync::oneshot::channel();
                                        pq.send(pq::Request::ProbeRoom{recipients:recipients.clone(),reply:checked});
                                        let legacy_tx=legacy.sender();
                                        tokio::spawn(async move {
                                            let result=crate::encryption_policy::select(
                                                async { response.await.unwrap_or(Err(crate::crypto::Error::Unavailable("OMEMO2 room check interrupted"))) },
                                                async {
                                                    let (checked,response)=tokio::sync::oneshot::channel();
                                                    legacy_tx.send(legacy::service::Request::ProbeRoom{recipients,reply:checked}).map_err(|_|crate::crypto::Error::Unavailable("Legacy room check interrupted"))?;
                                                    response.await.unwrap_or(Err(crate::crypto::Error::Unavailable("Legacy room check interrupted")))
                                                },
                                            ).await;
                                            let _=reply.send(result);
                                        });
                                    },
                                    RoomPreparation::Review=>legacy.send(legacy::service::Request::ReviewRoom{peer,members:recipients.members.into_keys().collect()}),
                                    RoomPreparation::PqReview=>pq.send(pq::Request::ReviewRoom{peer,members:recipients.members.into_keys().collect()}),
                                },
                                Err(error)=>{
                                    let text=error.to_string();
                                    match purpose {
                                        RoomPreparation::Send{message,..}=>emit(BackendEvent::Failed{peer:peer.clone(),id:message.wire_id}),
                                        RoomPreparation::Probe{reply}=>{let _=reply.send(Err(crate::crypto::Error::Unavailable("Could not establish complete room recipients. Join a nonanonymous members-only room and check its member permissions.")));},
                                        RoomPreparation::Review=>emit(BackendEvent::LegacyReview{peer:peer.clone(),rows:vec![],errors:vec![text.clone()]}),
                                        RoomPreparation::PqReview=>emit(BackendEvent::PqDevices{peer:peer.clone(),devices:vec![]}),
                                    }
                                    emit(BackendEvent::RoomError(text));
                                },
                            }
                        },
                        Some(Command::ReviewRoom{peer})=>prepare_room(&rooms,peer,room_epoch,RoomPreparation::Review,&network),
                        Some(Command::RoomWire{xml,peer,epoch,revision,reply})=>{
                            let allowed=online && epoch==room_epoch
                                && rooms.recipients(&peer).is_ok_and(|current|current.revision==revision)
                                && xml.is("message",NS) && xml.attr("type")==Some("groupchat")
                                && xml.attr("to")==Some(peer.as_str()) && xml.attr("from")==Some(owner.as_str())
                                && ((legacy_ready && xml.get_child("encrypted",legacy::NS).is_some()) || (pq_ready && xml.get_child("encrypted",crate::crypto::pq::NS).is_some()));
                            let _=reply.send(allowed && send(&mut client,xml).await);
                        },
                        Some(Command::JoinRoom{peer,nick})=>{
                            if !online {emit(BackendEvent::RoomError("Connect before joining a room".into()));continue}
                            if rooms.is_joined(&peer)||rooms.joining(&peer){continue}
                            let ticket=match room_jobs.begin(&peer){Ok(ticket)=>ticket,Err(_)=>continue};
                            let imported=rooms.imported_password(&peer);
                            let nick=nick.unwrap_or_else(||rooms.preferred_nick(&peer));
                            let (network,owner,epoch,events)=(network.clone(),owner.clone(),room_epoch,events.clone());
                            tokio::spawn(async move {
                                let result=crate::rooms::manage::join(&network,&owner,epoch,&peer,&nick,None,imported,&ticket).await;
                                let _=network.send(Command::RoomOperationFinished{peer,ticket});
                                if let Err(error)=result{let _=events.send((generation,owner,BackendEvent::RoomError(error.to_string())));}
                            });
                        },
                        Some(Command::Avatar { peer, source, reply })=>{
                            let client = crate::iq::Client::new(network.clone());
                            tokio::spawn(async move {
                                let result = if online { crate::avatar::fetch(&client, &peer, source).await }
                                    else { Err(crate::avatar::Error::Request(crate::iq::Error::Closed)) };
                                let _ = reply.send(result);
                            });
                        },
                        Some(Command::ChooseEncryption{peer,reply})=>{
                            if !online {
                                let _ = reply.send(Err(crate::crypto::Error::Unavailable("Connection interrupted")));
                                continue;
                            }
                            if rooms.known(&peer) {
                                prepare_room(&rooms,peer,room_epoch,RoomPreparation::Probe{reply},&network);
                                continue;
                            }
                            let (checked, first) = tokio::sync::oneshot::channel();
                            pq.send(pq::Request::Probe{peer:peer.clone(),reply:checked});
                            let legacy_tx = legacy.sender();
                            tokio::spawn(async move {
                                let result = crate::encryption_policy::select(
                                    async { first.await.unwrap_or(Err(crate::crypto::Error::Unavailable("OMEMO 2 check interrupted"))) },
                                    async {
                                        let (checked, response) = tokio::sync::oneshot::channel();
                                        legacy_tx.send(legacy::service::Request::Probe{peer,reply:checked})
                                            .map_err(|_| crate::crypto::Error::Unavailable("Legacy OMEMO check interrupted"))?;
                                        response.await.unwrap_or(Err(crate::crypto::Error::Unavailable("Legacy OMEMO check interrupted")))
                                    },
                                ).await;
                                let _ = reply.send(result);
                            });
                        },
                        Some(Command::Wire{xml,reply})=>{let ok=online && send(&mut client,xml).await;let _=reply.send(ok);},
                        Some(Command::Iq{to,kind,payload,reply})=>{
                            if !online{let _=reply.send(Err(crate::iq::Error::Closed));continue}
                            if let Some(root)=iq.start(to,kind,payload,reply){let id=root.attr("id").unwrap().to_string();if !send(&mut client,root).await{iq.fail(&id)}}
                        },
                        Some(Command::Send{peer,message,quote})=>{
                            if !online || message.unavailable || message.upload.is_some() || message.attachment.as_ref().is_some_and(|file|!file.valid_outgoing(message.encrypted)||message.poll.is_some()||message.action.is_some()){emit(BackendEvent::Failed{peer,id:message.wire_id});continue}
                            if rooms.known(&peer) {
                                if matches!(message.mode(),Mode::Legacy|Mode::Pq) {
                                    prepare_room(&rooms,peer,room_epoch,RoomPreparation::Send{message,quote},&network);
                                    continue;
                                }
                                match rooms.outgoing(&peer,&message,&quote){
                                    Ok(root)=>{let ok=send(&mut client,root).await;emit(if ok{BackendEvent::Sent{peer,id:message.wire_id}}else{BackendEvent::Failed{peer,id:message.wire_id}});}
                                    Err(error)=>{emit(BackendEvent::RoomError(error.to_string()));emit(BackendEvent::Failed{peer,id:message.wire_id});}
                                }
                                continue;
                            }
                            match message.mode() {
                                Mode::Legacy => { legacy.send(legacy::service::Request::Send{peer,message,quote}); continue; },
                                Mode::Omemo2 => { crypto.send(modern::Request::Send{peer,message,quote}); continue; },
                                Mode::Pq => { pq.send(pq::Request::Send{peer,message,quote}); continue; },
                                Mode::Plain => {},
                            }
                            let root=outgoing(&peer,&message,&quote);
                            let ok=send(&mut client,root).await;emit(if ok{BackendEvent::Sent{peer,id:message.wire_id}}else{BackendEvent::Failed{peer,id:message.wire_id}});
                        },
                        Some(Command::PreviewHistory{peer,start,end,reply})=>{
                            if reply.is_closed(){continue}
                            let room=rooms.known(&peer);
                            let interval=start.len()<=100&&end.len()<=100&&chrono::DateTime::parse_from_rfc3339(&start).ok().zip(chrono::DateTime::parse_from_rfc3339(&end).ok()).is_some_and(|(start,end)|start<=end);
                            if !online||!interval||bare(&peer).ok().as_deref()!=Some(peer.as_str())||(room&&!rooms.is_joined(&peer))||preview_active.len()>=4||preview_active.contains_key(&peer){let _=reply.send(false);continue}
                            let (id,root,page)=archive::Page::range(peer.clone(),room,String::new(),Some((&start,&end)),50);
                            if send(&mut client,root).await {
                                preview_active.insert(peer.clone(),id.clone());
                                preview_jobs.insert(id.clone(),PreviewHistoryJob {peer,start,end,epoch:room_epoch,token:id.clone(),cancel:preview_epoch.subscribe(),deadline:std::time::Instant::now()+Duration::from_secs(60),reply,pending:Vec::new(),cursors:Default::default(),results:Default::default(),page_count:0,failed:false});
                                mam.insert(id,page);
                            }else{let _=reply.send(false);}
                        },
                        Some(Command::PreviewHistoryNext{mut job,before,complete})=>{
                            let room=rooms.known(&job.peer);
                            if job.reply.is_closed()||preview_active.get(&job.peer)!=Some(&job.token)||!online||job.epoch!=room_epoch||job.deadline<=std::time::Instant::now()||job.failed||(room&&!rooms.is_joined(&job.peer)){finish_preview_history(&mut preview_active,job,false);continue}
                            if complete{finish_preview_history(&mut preview_active,job,true);continue}
                            if job.page_count==0||!job.cursors.insert(before.clone()){finish_preview_history(&mut preview_active,job,false);continue}
                            job.page_count=0;
                            let (id,root,page)=archive::Page::range(job.peer.clone(),room,before,Some((&job.start,&job.end)),50);
                            if send(&mut client,root).await {preview_jobs.insert(id.clone(),job);mam.insert(id,page);}else{finish_preview_history(&mut preview_active,job,false);}
                        },
                        Some(Command::History{peer,before})=>{
                            let room=rooms.known(&peer);
                            if !online || (room && !rooms.is_joined(&peer)) {
                                emit(archive::failed(peer));
                                continue;
                            }
                            let (id,root,page)=archive::Page::query(peer,room,before);
                            if send(&mut client,root).await {
                                mam.insert(id,page);
                            } else {
                                emit(archive::failed(page.peer));
                            }
                        },
                        Some(Command::Typing{peer,composing})=>{if online{send(&mut client,Element::builder("message",NS).attr("type".try_into().expect("static XML attribute"),"chat").attr("to".try_into().expect("static XML attribute"),peer).append(el(if composing{"composing"}else{"paused"},"http://jabber.org/protocol/chatstates")).build()).await;}},
                        Some(Command::ContactAccessState{peer,reply})=>{
                            let result=if rooms.known(&peer) {Err("A direct contact is required".into())}else{contacts.state(&peer)};
                            let _=reply.send(result);
                        },
                        Some(Command::ContactAccess{peer,action,reply})=>{
                            if reply.is_closed(){continue}
                            if !online || rooms.known(&peer) || !crate::contact_access::valid_peer(&owner,&peer){let _=reply.send(Err("Reconnect and choose a direct contact's bare address".into()));continue}
                            let (manager,network,epoch)=(contacts.clone(),network.clone(),contacts.epoch());
                            tokio::spawn(async move{let _=reply.send(manager.act(&network,epoch,&peer,action).await);});
                        },
                        Some(Command::ContactIq{epoch,kind,payload,reply})=>{
                            if reply.is_closed(){continue}
                            if !online || !contacts.valid(epoch) || !payload.is("query",crate::contact_access::ROSTER){let _=reply.send(Err(crate::iq::Error::Closed));continue}
                            let full=matches!(kind,crate::iq::Kind::Get);
                            let serial=if full{match contacts.begin_full(epoch){Ok(serial)=>Some(serial),Err(_)=>{let _=reply.send(Err(crate::iq::Error::Busy));continue}}}else{None};
                            let payload=if full{payload}else{contacts.roster_write(payload)};
                            if let Some(root)=iq.start(owner.clone(),kind,payload,reply){let id=root.attr("id").unwrap().to_string();if let Some(serial)=serial{contact_queries.insert(id.clone(),(epoch,std::time::Instant::now(),serial));}if !send(&mut client,root).await{contact_queries.remove(&id);if let Some(serial)=serial{contacts.clear_full(epoch,Some(serial));}iq.fail(&id)}}else if let Some(serial)=serial{contacts.clear_full(epoch,Some(serial));}
                        },
                        Some(Command::ContactWire{epoch,peer,kind,reply})=>{
                            let valid=online&&contacts.valid(epoch)&&!rooms.known(&peer)&&crate::contact_access::valid_peer(&owner,&peer)&&matches!(kind.as_str(),"subscribe"|"subscribed"|"unsubscribe"|"unsubscribed");
                            let result=valid&&send(&mut client,Element::builder("presence",NS).attr("to".try_into().unwrap(),peer).attr("type".try_into().unwrap(),kind).build()).await;
                            let _=reply.send(result);
                        },
                        Some(Command::Authorize{peer,allow,reply})=>{
                            if reply.is_closed(){continue}
                            if !online || rooms.known(&peer) || !crate::contact_access::valid_peer(&owner,&peer){let _=reply.send(Err("Reconnect and choose a direct contact's bare address".into()));continue}
                            let (manager,network,epoch)=(contacts.clone(),network.clone(),contacts.epoch());
                            tokio::spawn(async move{let action=if allow{crate::contact_access::Action::Accept}else{crate::contact_access::Action::Deny};let _=reply.send(manager.act(&network,epoch,&peer,action).await.map(|_|()));});
                        },
                        Some(Command::DisplayedPolicy{enabled,revision})=>{
                            if revision>displayed_policy.revision || revision==displayed_policy.revision&&!enabled {displayed_policy=crate::read_markers::Policy{enabled,revision};}
                            if online&&displayed_policy.enabled {let _=network.send(Command::FlushDisplayed);}
                        },
                        Some(Command::Displayed{peer,id})=>{
                            if online&&displayed_policy.enabled&&!rooms.known(&peer)&&peer!=owner {crate::read_markers::prepare(owner.clone(),peer,id,room_epoch,network.clone());}
                        },
                        Some(Command::DisplayedReady{candidate,epoch})=>{
                            let authorized=contacts.state(&candidate.peer).is_ok_and(|state|state.loaded&&state.shares_own_presence);
                            if online&&epoch==room_epoch&&displayed_policy.enabled&&candidate.revision==displayed_policy.revision&&!rooms.known(&candidate.peer)&&candidate.peer!=owner&&authorized {
                                if let Some(xml)=crate::read_markers::stanza(&candidate.peer,&candidate.wire){if send(&mut client,xml).await{crate::read_markers::acknowledge(owner.clone(),candidate);}}
                            }
                        },
                        Some(Command::FlushDisplayed)=>{if online&&displayed_policy.enabled{crate::read_markers::flush(owner.clone(),room_epoch,network.clone(),marker_flush.clone());}},
                        Some(Command::Receipt{peer,id})=>{if online{send(&mut client,Element::builder("message",NS).attr("to".try_into().expect("static XML attribute"),peer).attr("type".try_into().expect("static XML attribute"),"chat").append(Element::builder("received","urn:xmpp:receipts").attr("id".try_into().expect("static XML attribute"),id).build()).build()).await;}},
                    }},
                    event=client.next()=>{match event{
                        Some(Event::Online)=>{online=true;let contact_epoch=contacts.online();let (manager,contact_network)=(contacts.clone(),network.clone());tokio::spawn(async move{if let Err(error)=manager.refresh(&contact_network,contact_epoch).await{if manager.valid(contact_epoch){manager.error(error)}}});room_epoch=room_epoch.wrapping_add(1);preview_epoch.send_replace(room_epoch);let _=network.send(Command::LoadRoomBookmarks);crypto.send(modern::Request::Online(true));legacy.send(legacy::service::Request::Online(true));pq.send(pq::Request::Online(true));emit(BackendEvent::Status{online:true,error:false,text:"Connected · TLS".into()});send(&mut client,el("presence",NS)).await;send(&mut client,Element::builder("iq",NS).attr("type".try_into().expect("static XML attribute"),"set").attr("id".try_into().expect("static XML attribute"),"carbons").append(el("enable","urn:xmpp:carbons:2")).build()).await;},
                        Some(Event::Disconnected(error))=>{#[cfg(test)] eprintln!("fixture disconnect: {error:?}");online=false;preview_epoch.send_modify(|epoch|*epoch=epoch.wrapping_add(1));contacts.disconnect();contact_queries.clear();room_jobs.disconnect();rooms.disconnect();loading_rooms=false;subscribing_rooms=false;crypto_ready=false;legacy_ready=false;pq_ready=false;crypto.send(modern::Request::Online(false));legacy.send(legacy::service::Request::Online(false));pq.send(pq::Request::Online(false));iq.disconnect();for (_,job) in preview_jobs.drain(){finish_preview_history(&mut preview_active,job,false);}preview_active.clear();mam.clear();handshake=std::time::Instant::now();let fatal = fatal_error(&error);emit(BackendEvent::Status{online:false,error:fatal,text:if fatal{"Sign-in failed. Check credentials, TLS certificate and server configuration."}else{"Disconnected. Reconnecting…"}.into()});if fatal{break}},
                        None=>break,
                        Some(Event::Stanza(stanza))=>{
                            let root:Element=stanza.into();if iq.receive(&root,&owner){if let Some((epoch,_,serial))=contact_queries.remove(root.attr("id").unwrap_or("")){if root.attr("type")==Some("result")&&root.children().count()==1{if let Some(query)=root.get_child("query",crate::contact_access::ROSTER){if let Err(error)=crate::contact_access::parse_roster(query,true).and_then(|update|contacts.finish_full(epoch,serial,update)){contacts.clear_full(epoch,Some(serial));if contacts.valid(epoch){contacts.error(error)}}}else{contacts.clear_full(epoch,Some(serial));}}else{contacts.clear_full(epoch,Some(serial));}}continue}let from=root.attr("from").unwrap_or("");
                            if root.is("message",NS){
                                rooms.configuration_changed(&root);
                                refresh_room_members(&mut rooms,&bare_of(from),room_epoch,&network);
                                if crate::rooms::bookmark_changed(&root,&owner) { let _=network.send(Command::LoadRoomBookmarks); }
                                let mut results=root.children().filter(|child| child.is("result",archive::NS));
                                if let Some(wrapper)=results.next() {
                                    if results.next().is_some() { continue; }
                                    let query=wrapper.attr("queryid").unwrap_or("");
                                    let Some(page)=mam.get(query).filter(|page| page.accepts(&root,&owner)) else {continue};
                                    let Some(result)=page.result(wrapper) else {if let Some(job)=preview_jobs.get_mut(query){job.failed=true;}continue};
                                    if let Some(job)=preview_jobs.get_mut(query){
                                        if job.reply.is_closed(){continue}
                                        let stamp=chrono::DateTime::parse_from_rfc3339(result.stamp).ok();
                                        let in_range=stamp.zip(chrono::DateTime::parse_from_rfc3339(&job.start).ok()).zip(chrono::DateTime::parse_from_rfc3339(&job.end).ok()).is_some_and(|((stamp,start),end)|stamp>=start&&stamp<=end);
                                        if job.epoch!=room_epoch||job.deadline<=std::time::Instant::now()||job.page_count>=50||!archive::valid_uid(result.uid)||!in_range||(page.room&&!rooms.is_joined(&page.peer)){job.failed=true;continue}
                                        if !job.results.insert(result.uid.to_string()){continue}
                                        job.page_count+=1;
                                        if page.room {
                                            if result.xml.attr("type")!=Some("groupchat")||bare_of(result.xml.attr("from").unwrap_or(""))!=page.peer {job.failed=true;continue}
                                        }else{
                                            let Some((peer,_,_))=normalize(result.xml,&owner,Some((result.uid,result.stamp)))else{continue};
                                            if peer!=page.peer{job.failed=true;continue}
                                        }
                                        match preview_metadata(&rooms,&crypto,&legacy,&pq,&emit,result.xml,&owner,(result.uid,result.stamp)){
                                            Ok(Some(reply))=>job.pending.push(reply),Ok(None)=>{},Err(())=>job.failed=true,
                                        }
                                        continue;
                                    }
                                    if page.room {
                                        deliver_room(&rooms,&legacy,&pq,&emit,result.xml,&owner,Some((result.uid,result.stamp)));
                                    } else if normalize(result.xml,&owner,Some((result.uid,result.stamp))).is_some_and(|(peer,_,_)| peer==page.peer) {
                                        deliver(&crypto,&legacy,&pq,&emit,result.xml,&owner,Some((result.uid,result.stamp)),true,None);
                                    }
                                    continue;
                                }
                                if crate::rooms::invitations::recognized(&root) {
                                    if let Some(invite) = crate::rooms::invitations::parse(&root, &owner) {
                                        if !rooms.is_joined(&invite.room) { emit(BackendEvent::RoomInvitation(invite)); }
                                    }
                                    continue;
                                }
                                if root.attr("type")==Some("groupchat"){
                                    deliver_room(&rooms,&legacy,&pq,&emit,&root,&owner,None);
                                    continue;
                                }
                                if crate::avatar::pep_changed(&root) && bare(from).ok().as_deref()==Some(from) {
                                    emit(BackendEvent::AvatarChanged{peer:from.into(),hint:None});
                                }
                                if root.attr("type")==Some("error"){emit(BackendEvent::Failed{peer:bare_of(from),id:root.attr("id").unwrap_or("").into()});continue}
                                if let Some(notice)=crate::read_markers::incoming_receipt(&root,&owner).filter(|n|!rooms.known(&n.peer)){emit(BackendEvent::Receipt{peer:notice.peer,id:notice.id});}
                                if let Some(notice)=crate::read_markers::incoming(&root,&owner).filter(|n|!rooms.known(&n.peer)){emit(BackendEvent::Read{peer:notice.peer,id:notice.id});}
                                let mut carbon_wrappers=root.children().filter(|c|c.is("received","urn:xmpp:carbons:2")||c.is("sent","urn:xmpp:carbons:2"));
                                if let Some(wrapper)=carbon_wrappers.next(){
                                    if carbon_wrappers.next().is_some()||from!=owner||root.attr("to").is_some_and(|to|bare_of(to)!=owner){continue}
                                    if let Some((inner,historical))=carbon_payload(wrapper){
                                        if wrapper.is("sent","urn:xmpp:carbons:2"){
                                            if let Some(notice)=crate::read_markers::own_displayed(inner,&owner).filter(|n|!rooms.known(&n.peer)){emit(BackendEvent::OwnRead{peer:notice.peer,id:notice.id});}
                                        }else{
                                            if let Some(notice)=crate::read_markers::incoming_receipt(inner,&owner).filter(|n|!rooms.known(&n.peer)){emit(BackendEvent::Receipt{peer:notice.peer,id:notice.id});}
                                            if let Some(notice)=crate::read_markers::incoming(inner,&owner).filter(|n|!rooms.known(&n.peer)){emit(BackendEvent::Read{peer:notice.peer,id:notice.id});}
                                        }
                                        deliver(&crypto,&legacy,&pq,&emit,inner,&owner,None,historical,None);
                                    }continue
                                }
                                if let Some((_peer,message,historical))=normalize(&root,&owner,None){let receipt_to=(!historical&&!message.own&&!message.wire_id.is_empty()&&root.get_child("request","urn:xmpp:receipts").is_some()).then(||from.to_string());deliver(&crypto,&legacy,&pq,&emit,&root,&owner,None,historical,receipt_to);}
                                if root.get_child("delay","urn:xmpp:delay").is_none(){for name in ["composing","paused","active","inactive","gone"]{if root.get_child(name,"http://jabber.org/protocol/chatstates").is_some(){emit(BackendEvent::Typing{peer:bare_of(from),composing:name=="composing"})}}}
                            }else if root.is("presence",NS){
                                let room_presence=rooms.presence(&root);
                                room_jobs.changed(&rooms,&bare_of(from));
                                refresh_room_members(&mut rooms,&bare_of(from),room_epoch,&network);
                                if let Some(presence)=room_presence{
                                    if !presence.joined {
                                        mam.retain(|id,page| {
                                            let left=page.room && page.peer==presence.jid;
                                            if left {if let Some(job)=preview_jobs.remove(id){finish_preview_history(&mut preview_active,job,false);}else{emit(archive::failed(page.peer.clone()));}}
                                            !left
                                        });
                                    }
                                    emit(BackendEvent::RoomPresence(presence));
                                    continue;
                                }
                                if root.get_child("x","http://jabber.org/protocol/muc#user").is_some(){continue;}
                                let peer=bare_of(from);let kind=root.attr("type").unwrap_or("");
                                if kind.is_empty(){if let Some(hint)=crate::avatar::presence_hash(&root){if bare(&peer).ok().as_deref()==Some(&peer){emit(BackendEvent::AvatarChanged{peer:peer.clone(),hint:Some(hint)});}}}
                                if matches!(kind,"subscribe"|"unsubscribe"|"subscribed"|"unsubscribed"){
                                    if let Some(peer)=subscription_peer(&root,&owner).filter(|peer|!rooms.known(peer)){
                                        match kind{
                                            "subscribe"=>{contacts.subscription(&peer,true);emit(BackendEvent::Request(peer.clone()));let (manager,network,epoch)=(contacts.clone(),network.clone(),contacts.epoch());tokio::spawn(async move{if let Err(error)=manager.approve_if_allowed(&network,epoch,&peer).await{if manager.valid(epoch){manager.error(error)}}});},
                                            "unsubscribe"=>{contacts.subscription(&peer,false);emit(BackendEvent::RequestCancelled(peer));},
                                            "unsubscribed"=>contacts.revoked_by_contact(&peer),
                                            _=>{},
                                        }
                                    }
                                    continue;
                                }
                                if !rooms.known(&peer){contacts.presence(&root);}
                            }else if root.is("iq",NS){let id=root.attr("id").unwrap_or("");let trusted=from.is_empty()||from==owner;
                                if trusted&&!id.is_empty()&&root.children().count()==1&&root.attr("type")==Some("set")&&root.attr("to").is_none_or(|to|to.parse::<tokio_xmpp::jid::Jid>().ok().is_some_and(|jid|jid.to_bare().as_str()==owner)){
                                    if let Some(query)=root.get_child("query",crate::contact_access::ROSTER){
                                        match crate::contact_access::parse_roster(query,false).and_then(|update|contacts.apply_roster(contacts.epoch(),update)){
                                            Ok(())=>{send(&mut client,Element::builder("iq",NS).attr("type".try_into().unwrap(),"result").attr("id".try_into().unwrap(),id).build()).await;},
                                            Err(error)=>contacts.error(error),
                                        }
                                    }
                                }
                                if let Some(page)=mam.get(id) {
                                    if page.accepts(&root,&owner) && matches!(root.attr("type"),Some("result"|"error")) {
                                        let page=mam.remove(id).expect("matched archive query");
                                        if let Some(job)=preview_jobs.remove(id){
                                            if let BackendEvent::History{before,complete,error,..}=page.finish(&root){
                                                let explicit_complete=root.get_child("fin",archive::NS).is_some_and(|fin|matches!(fin.attr("complete"),Some("true"|"1")));
                                                if error||(before.is_empty()&&!explicit_complete){finish_preview_history(&mut preview_active,job,false);}else{let next_network=network.clone();tokio::spawn(async move{preview_history_page(&next_network,job,before,complete).await;});}
                                            }
                                        }else{emit(page.finish(&root));}
                                    }
                                    continue;
                                }
                                if root.attr("type")==Some("get")&&root.get_child("query","http://jabber.org/protocol/disco#info").is_some(){let mut query=el("query","http://jabber.org/protocol/disco#info");for ns in ["urn:xmpp:receipts",crate::read_markers::NS,REPLY,SID,"http://jabber.org/protocol/chatstates",crate::message_actions::CORRECT,crate::message_actions::REACTIONS,crate::message_actions::RETRACT,crate::polls::NS,crate::forwarding::NS,crate::file_share::NS,crate::rooms::invitations::DIRECT]{query.append_child(Element::builder("feature","http://jabber.org/protocol/disco#info").attr("var".try_into().expect("static XML attribute"),ns).build());}if legacy_ready{query.append_child(Element::builder("feature","http://jabber.org/protocol/disco#info").attr("var".try_into().unwrap(),legacy::NS).build());}if crypto_ready{query.append_child(Element::builder("feature","http://jabber.org/protocol/disco#info").attr("var".try_into().unwrap(),"urn:xmpp:omemo:2").build());}if pq_ready {for suffix in ["",":devices+notify",":bundles+notify",":pqxdh",":spqr",":bundle-v4",":kex-auth"]{query.append_child(Element::builder("feature","http://jabber.org/protocol/disco#info").attr("var".try_into().unwrap(),format!("{}{suffix}",crate::crypto::pq::NS)).build());}}send(&mut client,Element::builder("iq",NS).attr("to".try_into().expect("static XML attribute"),from).attr("type".try_into().expect("static XML attribute"),"result").attr("id".try_into().expect("static XML attribute"),id).append(query).build()).await;}
                            }
                        }
                    }},
                    _=timer.tick()=>{if online&&displayed_policy.enabled&&marker_tick.elapsed()>=Duration::from_secs(5){marker_tick=std::time::Instant::now();let _=network.send(Command::FlushDisplayed);}iq.expire();contact_queries.retain(|_,(epoch,started,serial)|{let keep=contacts.valid(*epoch)&&started.elapsed()<Duration::from_secs(25);if !keep{contacts.clear_full(*epoch,Some(*serial));}keep});for stanza in room_jobs.expire(&mut rooms){let peer=bare_of(stanza.attr("to").unwrap_or(""));send(&mut client,stanza).await;emit(BackendEvent::RoomPresence(rooms.status(&peer)));}for presence in rooms.expire(){emit(BackendEvent::RoomPresence(presence));}if !online&&handshake.elapsed()>Duration::from_secs(45){emit(BackendEvent::Status{online:false,error:true,text:"Connection timed out. Check the account settings.".into()});break}mam.retain(|id,page| {
                        let context_expired=preview_jobs.get(id).is_some_and(|job|job.reply.is_closed()||job.epoch!=room_epoch||job.deadline<=std::time::Instant::now());
                        let expired=page.started.elapsed()>Duration::from_secs(20)||context_expired;
                        if expired {if let Some(job)=preview_jobs.remove(id){finish_preview_history(&mut preview_active,job,false);}else{emit(archive::failed(page.peer.clone()));}}
                        !expired
                    });}
                }}
                preview_epoch.send_modify(|epoch|*epoch=epoch.wrapping_add(1));for (_,job) in preview_jobs.drain(){finish_preview_history(&mut preview_active,job,false);}preview_active.clear();contacts.disconnect();contact_queries.clear();room_jobs.disconnect();iq.disconnect();
                let _=tokio::time::timeout(Duration::from_secs(2),client.send_end()).await;
            } => {} } });
        }).expect("network thread");
        Self {
            tx,
            cancel: Some(cancel),
        }
    }
    pub fn send(&self, c: Command) -> bool {
        self.tx.send(c).is_ok()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}
fn fatal_error(error: &tokio_xmpp::Error) -> bool {
    use tokio_xmpp::Error;
    match error {
        Error::Auth(_) | Error::Protocol(_) => true,
        Error::Connection(e) => {
            let e: &(dyn std::error::Error + 'static) = e.as_ref();
            e.is::<tokio_xmpp::connect::tls_common::TlsConnectorError>()
        }
        Error::Io(e) => matches!(
            e.kind(),
            std::io::ErrorKind::InvalidData | std::io::ErrorKind::PermissionDenied
        ),
        _ => false,
    }
}
fn preview_metadata(
    rooms: &crate::rooms::Rooms,
    crypto: &modern::Handle,
    legacy: &legacy::service::Handle,
    pq: &pq::Handle,
    emit: &impl Fn(BackendEvent),
    xml: &Element,
    owner: &str,
    archive: (&str, &str),
) -> Result<Option<tokio::sync::oneshot::Receiver<bool>>, ()> {
    if xml.attr("type") == Some("groupchat") {
        let room = bare_of(xml.attr("from").unwrap_or(""));
        let Some(mut metadata) = rooms.archived_message(
            &room,
            archive::ResultStanza {
                xml,
                uid: archive.0,
                stamp: archive.1,
            },
        ) else {
            return Ok(None);
        };
        if xml.get_child("encrypted", crate::crypto::pq::NS).is_some() {
            let sender = rooms.sender(xml, true).map_err(|_| ())?;
            metadata.own = sender.author == owner;
            let frame = modern::Frame { xml: xml.clone(), archive: Some((archive.0.into(), archive.1.into())), historical: true, receipt_to: None };
            return pq.preview_with_room(frame, Some(crate::crypto::journal::RoomContext { sender, metadata })).map(Some).ok_or(());
        }
        if xml.get_child("encrypted", legacy::NS).is_some() {
            let sender = rooms.sender(xml, true).map_err(|_| ())?;
            metadata.own = sender.author == owner;
            metadata.legacy_room_sender = Some(sender.clone());
            let frame = modern::Frame {
                xml: xml.clone(),
                archive: Some((archive.0.into(), archive.1.into())),
                historical: true,
                receipt_to: None,
            };
            return legacy
                .preview(
                    frame,
                    Some(crate::crypto::journal::RoomContext { sender, metadata }),
                )
                .map(Some)
                .ok_or(());
        }
        emit(BackendEvent::Message {
            peer: room,
            message: metadata,
            historical: true,
            receipt_to: None,
        });
        return Ok(None);
    }
    let modern = xml.get_child("encrypted", "urn:xmpp:omemo:2").is_some();
    let classic = xml.get_child("encrypted", legacy::NS).is_some();
    let post_quantum = xml.get_child("encrypted", crate::crypto::pq::NS).is_some();
    if u8::from(modern) + u8::from(classic) + u8::from(post_quantum) == 1 {
        let frame = modern::Frame {
            xml: xml.clone(),
            archive: Some((archive.0.into(), archive.1.into())),
            historical: true,
            receipt_to: None,
        };
        return if post_quantum {
            pq.preview(frame)
        } else if modern {
            crypto.preview(frame)
        } else {
            legacy.preview(frame, None)
        }
        .map(Some)
        .ok_or(());
    }
    deliver(crypto, legacy, pq, emit, xml, owner, Some(archive), true, None);
    Ok(None)
}
fn deliver_room(
    rooms: &crate::rooms::Rooms,
    legacy: &legacy::service::Handle,
    pq: &pq::Handle,
    emit: &impl Fn(BackendEvent),
    xml: &Element,
    owner: &str,
    archive: Option<(&str, &str)>,
) {
    let parsed = if let Some((uid, stamp)) = archive {
        let room = bare_of(xml.attr("from").unwrap_or(""));
        rooms
            .archived_message(&room, archive::ResultStanza { xml, uid, stamp })
            .map(|message| (room, message, true))
    } else {
        rooms.message(xml)
    };
    let Some((peer, mut metadata, historical)) = parsed else {
        return;
    };
    if xml.get_child("encrypted", crate::crypto::pq::NS).is_some() {
        if let Ok(sender) = rooms.sender(xml, archive.is_some()) {
            metadata.own = sender.author == owner;
            if metadata.own { metadata.author = "You".into(); }
            pq.send(pq::Request::IncomingRoom {
                frame: modern::Frame { xml: xml.clone(), archive: archive.map(|(id,stamp)|(id.into(),stamp.into())), historical, receipt_to: None },
                sender, metadata,
            });
            return;
        }
        metadata.text = "The room did not disclose this sender's identity. Refresh room history after reconnecting.".into();
        metadata.set_encryption(Mode::Pq, String::new());
        metadata.ciphertext = crate::crypto::bundle::xml(xml, crate::crypto::pq::wire::MAX_WIRE).ok().and_then(|xml|String::from_utf8(xml).ok()).unwrap_or_default();
    }
    if xml.get_child("encrypted", legacy::NS).is_some() {
        if let Ok(sender) = rooms.sender(xml, archive.is_some()) {
            metadata.own = sender.author == owner;
            metadata.author = if metadata.own {
                "You".into()
            } else {
                metadata.author
            };
            metadata.legacy_room_sender = Some(sender.clone());
            legacy.send(legacy::service::Request::IncomingRoom {
                frame: modern::Frame {
                    xml: xml.clone(),
                    archive: archive.map(|(id, stamp)| (id.into(), stamp.into())),
                    historical,
                    receipt_to: None,
                },
                sender,
                metadata,
            });
            return;
        }
        metadata.text = "The room did not disclose this sender's identity. Refresh room history after reconnecting.".into();
        metadata.set_encryption(Mode::Legacy, String::new());
        metadata.ciphertext = crate::crypto::bundle::xml(xml, 1024 * 1024)
            .ok()
            .and_then(|xml| String::from_utf8(xml).ok())
            .unwrap_or_default();
    }
    emit(BackendEvent::Message {
        peer,
        message: metadata,
        historical,
        receipt_to: None,
    });
}
fn deliver(
    crypto: &modern::Handle,
    legacy: &legacy::service::Handle,
    pq: &pq::Handle,
    emit: &impl Fn(BackendEvent),
    xml: &Element,
    owner: &str,
    archive: Option<(&str, &str)>,
    historical: bool,
    receipt_to: Option<String>,
) {
    match crate::message_actions::public_legacy_content(xml) {
        Ok(Some(clear)) => {
            if let Some((peer, message, delayed)) = normalize(&clear, owner, archive) {
                emit(BackendEvent::Message {
                    peer,
                    message,
                    historical: historical || delayed,
                    receipt_to: None,
                });
            }
            return;
        }
        Err(()) => return,
        Ok(None) => {}
    }
    let modern = xml.get_child("encrypted", "urn:xmpp:omemo:2").is_some();
    let classic = xml.get_child("encrypted", legacy::NS).is_some();
    let post_quantum = xml.get_child("encrypted", crate::crypto::pq::NS).is_some();
    if u8::from(modern) + u8::from(classic) + u8::from(post_quantum) == 1 {
        let frame = modern::Frame {
            xml: xml.clone(),
            archive: archive.map(|(id, stamp)| (id.into(), stamp.into())),
            historical,
            receipt_to,
        };
        if post_quantum {
            pq.send(pq::Request::Incoming(frame));
        } else if modern {
            crypto.send(modern::Request::Incoming(frame));
        } else {
            legacy.send(legacy::service::Request::Incoming(frame));
        }
    } else if let Some((peer, message, delayed)) = normalize(xml, owner, archive) {
        emit(BackendEvent::Message {
            peer,
            message,
            historical: historical || delayed,
            receipt_to: if modern { None } else { receipt_to },
        });
    }
}
async fn send(client: &mut Client, root: Element) -> bool {
    let Ok(stanza) = Stanza::try_from(root) else {
        return false;
    };
    tokio::time::timeout(Duration::from_secs(15), client.send_stanza(stanza))
        .await
        .is_ok_and(|r| r.is_ok())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pq_messages_are_visible_without_trusting_outside_content() {
        for body in ["", "<body>Untrusted outside text</body><x xmlns='jabber:x:oob'><url>https://example.org/fake.png</url></x>"] {
            let xml: Element = format!("<message xmlns='jabber:client' id='pq-message' type='chat' from='friend@example.org/phone' to='me@example.org'><encrypted xmlns='urn:monocles:omemo-pq:1'/>{body}</message>").parse().unwrap();
            let (_, message, _) = normalize(&xml, "me@example.org", None).unwrap();
            assert!(message.unavailable);
            assert!(message.text.starts_with("Encrypted message"));
            assert!(message.attachment.is_none());
        }
    }
    #[test]
    fn live_carbons_are_not_archive_messages() {
        for delay in ["", "<delay xmlns='urn:xmpp:delay' stamp='2026-11-11T18:00:00Z'/>"] {
            let wrapper: Element = format!("<sent xmlns='urn:xmpp:carbons:2'><forwarded xmlns='urn:xmpp:forward:0'>{delay}<message xmlns='jabber:client' from='me@example.org/phone' to='friend@example.org' type='chat'><body>From phone</body></message></forwarded></sent>").parse().unwrap();
            let (inner, historical) = carbon_payload(&wrapper).unwrap();
            let (_, message, delayed) = normalize(inner, "me@example.org", None).unwrap();
            assert!(message.own);
            assert_eq!(historical || delayed, !delay.is_empty());
        }
    }
    #[test]
    fn preview_history_waits_for_metadata_workers_without_using_the_chat_cursor() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (network, mut commands) = channel::unbounded_channel();
            let (reply, completion) = tokio::sync::oneshot::channel();
            let (ack, received) = tokio::sync::oneshot::channel();
            let (epoch, cancel) = tokio::sync::watch::channel(1u64);
            let job = PreviewHistoryJob {
                peer: "peer@example.org".into(),
                start: "2026-11-11T00:00:00Z".into(),
                end: "2026-11-11T23:59:59Z".into(),
                epoch: 1,
                token: "metadata-only".into(),
                cancel,
                deadline: std::time::Instant::now() + Duration::from_secs(2),
                reply,
                pending: vec![received],
                cursors: Default::default(),
                results: Default::default(),
                page_count: 1,
                failed: false,
            };
            let work = tokio::spawn(async move {
                preview_history_page(&network, job, "archive-first".into(), true).await;
            });
            tokio::task::yield_now().await;
            assert!(commands.try_recv().is_err());
            ack.send(true).unwrap();
            work.await.unwrap();
            let Command::PreviewHistoryNext {
                job,
                before,
                complete,
            } = commands.recv().await.unwrap()
            else {
                panic!("metadata refresh changed ordinary history cursor")
            };
            assert_eq!(before, "archive-first");
            assert!(complete && !job.failed);
            let mut active = HashMap::from([("peer@example.org".into(), "newer-job".into())]);
            finish_preview_history(&mut active, job, false);
            assert!(!completion.await.unwrap());
            assert_eq!(
                active.get("peer@example.org").map(String::as_str),
                Some("newer-job")
            );
            let (_, request, _) = archive::Page::range(
                "peer@example.org".into(),
                false,
                String::new(),
                Some(("2026-11-11T00:00:00Z", "2026-11-11T23:59:59Z")),
                50,
            );
            let form = request
                .get_child("query", archive::NS)
                .unwrap()
                .get_child("x", "jabber:x:data")
                .unwrap();
            for name in ["with", "start", "end"] {
                assert!(form.children().any(|field| field.attr("var") == Some(name)));
            }
            drop(epoch);
        });
    }
    #[test]
    fn subscriptions_require_a_real_bare_contact_for_this_account() {
        for kind in ["subscribe", "unsubscribe", "subscribed", "unsubscribed"] {
            let xml: Element = format!("<presence xmlns='jabber:client' type='{kind}' from='friend@example.org' to='me@example.org/desktop'/>").parse().unwrap();
            assert_eq!(
                subscription_peer(&xml, "me@example.org").as_deref(),
                Some("friend@example.org")
            );
            for from in ["friend@example.org/phone", "example.org", "me@example.org"] {
                let mut invalid = xml.clone();
                invalid.set_attr("".into(), "from".try_into().unwrap(), from);
                assert!(subscription_peer(&invalid, "me@example.org").is_none());
            }
            for namespace in [
                "urn:xmpp:delay",
                "jabber:x:delay",
                "urn:xmpp:carbons:2",
                "urn:xmpp:forward:0",
                "urn:xmpp:mam:2",
            ] {
                let mut delayed = xml.clone();
                delayed.append_child(Element::builder("wrapper", namespace).build());
                assert!(subscription_peer(&delayed, "me@example.org").is_none());
            }
            let mut wrong_account = xml;
            wrong_account.set_attr("".into(), "to".try_into().unwrap(), "other@example.org");
            assert!(subscription_peer(&wrong_account, "me@example.org").is_none());
        }
    }
    #[test]
    fn invitations_never_become_fallback_direct_messages() {
        let direct: Element = "<message xmlns='jabber:client' type='chat' from='friend@example.org' to='me@example.org'><body>Invitation fallback</body><x xmlns='jabber:x:conference' jid='room@conference.example.org'/></message>".parse().unwrap();
        assert!(normalize(&direct, "me@example.org", None).is_none());
        assert!(
            normalize(
                &direct,
                "me@example.org",
                Some(("archive-id", "2026-11-11T12:00:00Z"))
            )
            .is_none()
        );
    }
    #[test]
    fn empty_ids_are_not_shared_aliases() {
        let xml:Element="<message xmlns='jabber:client' from='b@example.org' to='a@example.org' id=''><body>Keep this message</body><origin-id xmlns='urn:xmpp:sid:0' id=''/><stanza-id xmlns='urn:xmpp:sid:0' by='a@example.org' id=''/></message>".parse().unwrap();
        let (_, message, _) = normalize(&xml, "a@example.org", Some(("", ""))).unwrap();
        assert!(message.aliases.is_empty());
        assert!(message.wire_id.is_empty());
        assert_eq!(message.text, "Keep this message");
    }
    #[test]
    fn reply_unicode_and_bad_ranges() {
        let e:Element="<message xmlns='jabber:client'><reply xmlns='urn:xmpp:reply:0' id='one' to='a@example.org'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'><body start='0' end='4'/></fallback></message>".parse().unwrap();
        let (_, _, q, t) = reply_content(&e, "> 🌿\nreply");
        assert_eq!(q, "> 🌿");
        assert_eq!(t, "reply");
        let bad:Element="<message xmlns='jabber:client'><reply xmlns='urn:xmpp:reply:0' id='one'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'><body start='-1' end='20'/></fallback></message>".parse().unwrap();
        assert_eq!(reply_content(&bad, "keep me").3, "keep me");
    }
    #[test]
    fn encrypted_fallback_and_foreign_account() {
        let e:Element="<message xmlns='jabber:client' from='b@example.org/r' to='a@example.org' type='chat' id='one'><body>not decrypted</body><encrypted xmlns='urn:xmpp:omemo:2'/></message>".parse().unwrap();
        let (_, m, _) = normalize(&e, "a@example.org", None).unwrap();
        assert!(m.unavailable && !m.encrypted);
        assert!(!m.text.contains("not decrypted"));
        assert!(normalize(&e, "other@example.org", None).is_none());
    }
}
