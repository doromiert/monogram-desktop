//! Standards-only XMPP boundary. No GTK, database access, or implicit plaintext fallback.
use crate::model::{Account, Message, new_id};
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
pub fn el(name: &str, ns: &str) -> Element {
    Element::builder(name, ns).build()
}
pub fn reply_content(xml: &Element, body: &str) -> (String, String, String, String) {
    let replies: Vec<_> = xml.children().filter(|c| c.is("reply", REPLY)).collect();
    if replies.len() != 1 {
        return empty_reply(body);
    }
    let r = replies[0];
    let id = r.attr("id").unwrap_or("");
    if !crate::message_actions::valid_id(id) {
        return empty_reply(body);
    }
    let author = bare_of(r.attr("to").unwrap_or(""));
    if !author.is_empty() && bare(&author).is_err() {
        return empty_reply(body);
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
        return (id.into(), author, String::new(), body.into());
    }
    ranges.sort();
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
    (id.into(), author, quote.trim().into(), clean)
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
    let unavailable = xml.children().any(|c| {
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
    let raw = if unavailable {
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
    root
}
#[derive(Debug, Clone)]
pub enum BackendEvent {
    Uncertain {
        peer: String,
        id: String,
    },
    CryptoStatus(crate::crypto::service::Status),
    CryptoReview {
        peer: String,
        rows: Vec<crate::crypto::service::Review>,
        errors: Vec<String>,
    },
    CryptoError {
        peer: String,
        text: String,
    },
    CryptoAliases {
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
    Status {
        online: bool,
        error: bool,
        text: String,
    },
    Roster(Vec<(String, String, String)>),
    Presence {
        peer: String,
        online: bool,
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
}
#[derive(Debug)]
pub enum Command {
    CryptoReady(bool),
    Crypto(crate::crypto::service::Request),
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
    Typing {
        peer: String,
        composing: bool,
    },
    Authorize {
        peer: String,
        allow: bool,
    },
    Receipt {
        peer: String,
        id: String,
    },
}
pub type Envelope = (u64, String, BackendEvent);
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
        let (tx, mut rx) = channel::unbounded_channel();
        let (cancel, stopped) = tokio::sync::oneshot::channel();
        let network = tx.clone();
        thread::Builder::new().name("monogram-xmpp".into()).spawn(move||{
            let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().expect("network runtime");
            runtime.block_on(async move{ tokio::select! { _ = stopped => {}, _ = async move {
                let owner=config.jid.clone();let emit=|e|{let _=events.send((generation,owner.clone(),e));};
                let Ok(_jid)=bare(&config.jid)else{emit(BackendEvent::Status{online:false,error:true,text:"Invalid XMPP address".into()});return};
                let mut client = Client::new(config, password);
                let crypto=crate::crypto::service::Handle::start(owner.clone(),generation,network,events.clone());
                let mut iq=crate::iq::Pending::default();
                let mut crypto_ready=false;let mut online=false;let mut mam:HashMap<String,(String,std::time::Instant)>=HashMap::new();let mut resources:HashMap<String,std::collections::HashSet<String>>=HashMap::new();let mut timer=tokio::time::interval(Duration::from_secs(1));let mut handshake=std::time::Instant::now();
                emit(BackendEvent::Status{online:false,error:false,text:"Connecting…".into()});
                loop{tokio::select!{
                    command=rx.recv()=>{match command{
                        None=>break,
                        Some(Command::CryptoReady(ready))=>crypto_ready=ready,
                        Some(Command::Crypto(request))=>crypto.send(request),
                        Some(Command::Wire{xml,reply})=>{let ok=online && send(&mut client,xml).await;let _=reply.send(ok);},
                        Some(Command::Iq{to,kind,payload,reply})=>{
                            if !online{let _=reply.send(Err(crate::iq::Error::Closed));continue}
                            if let Some(root)=iq.start(to,kind,payload,reply){let id=root.attr("id").unwrap().to_string();if !send(&mut client,root).await{iq.fail(&id)}}
                        },
                        Some(Command::Send{peer,message,quote})=>{
                            if !online || message.unavailable || message.upload.is_some() || message.attachment.as_ref().is_some_and(|file|!file.valid_outgoing(message.encrypted)||message.poll.is_some()||message.action.is_some()){emit(BackendEvent::Failed{peer,id:message.wire_id});continue}
                            if message.encrypted{crypto.send(crate::crypto::service::Request::Send{peer,message,quote});continue}
                            let root=outgoing(&peer,&message,&quote);
                            let ok=send(&mut client,root).await;emit(if ok{BackendEvent::Sent{peer,id:message.wire_id}}else{BackendEvent::Failed{peer,id:message.wire_id}});
                        },
                        Some(Command::History{peer,before})=>{
                            if !online{emit(BackendEvent::History{peer,before:String::new(),complete:false,error:true});continue}
                            let id=new_id();let query=Element::builder("query","urn:xmpp:mam:2").attr("queryid".try_into().expect("static XML attribute"),&id).append(Element::builder("x","jabber:x:data").attr("type".try_into().expect("static XML attribute"),"submit").append(Element::builder("field","jabber:x:data").attr("var".try_into().expect("static XML attribute"),"FORM_TYPE").attr("type".try_into().expect("static XML attribute"),"hidden").append(Element::builder("value","jabber:x:data").append("urn:xmpp:mam:2").build()).build()).append(Element::builder("field","jabber:x:data").attr("var".try_into().expect("static XML attribute"),"with").append(Element::builder("value","jabber:x:data").append(peer.clone()).build()).build()).build()).append(Element::builder("set","http://jabber.org/protocol/rsm").append(Element::builder("max","http://jabber.org/protocol/rsm").append("50").build()).append(Element::builder("before","http://jabber.org/protocol/rsm").append(before).build()).build()).build();
                            mam.insert(id.clone(),(peer,std::time::Instant::now()));send(&mut client,Element::builder("iq",NS).attr("type".try_into().expect("static XML attribute"),"set").attr("id".try_into().expect("static XML attribute"),id).append(query).build()).await;
                        },
                        Some(Command::Typing{peer,composing})=>{if online{send(&mut client,Element::builder("message",NS).attr("type".try_into().expect("static XML attribute"),"chat").attr("to".try_into().expect("static XML attribute"),peer).append(el(if composing{"composing"}else{"paused"},"http://jabber.org/protocol/chatstates")).build()).await;}},
                        Some(Command::Authorize{peer,allow})=>{if online{send(&mut client,Element::builder("presence",NS).attr("to".try_into().expect("static XML attribute"),peer).attr("type".try_into().expect("static XML attribute"),if allow{"subscribed"}else{"unsubscribed"}).build()).await;}},
                        Some(Command::Receipt{peer,id})=>{if online{send(&mut client,Element::builder("message",NS).attr("to".try_into().expect("static XML attribute"),peer).attr("type".try_into().expect("static XML attribute"),"chat").append(Element::builder("received","urn:xmpp:receipts").attr("id".try_into().expect("static XML attribute"),id).build()).build()).await;}},
                    }},
                    event=client.next()=>{match event{
                        Some(Event::Online)=>{online=true;crypto.send(crate::crypto::service::Request::Online(true));emit(BackendEvent::Status{online:true,error:false,text:"Connected · TLS".into()});send(&mut client,el("presence",NS)).await;send(&mut client,Element::builder("iq",NS).attr("type".try_into().expect("static XML attribute"),"get").attr("id".try_into().expect("static XML attribute"),"roster").append(el("query","jabber:iq:roster")).build()).await;send(&mut client,Element::builder("iq",NS).attr("type".try_into().expect("static XML attribute"),"set").attr("id".try_into().expect("static XML attribute"),"carbons").append(el("enable","urn:xmpp:carbons:2")).build()).await;},
                        Some(Event::Disconnected(error))=>{#[cfg(test)] eprintln!("fixture disconnect: {error:?}");online=false;crypto_ready=false;crypto.send(crate::crypto::service::Request::Online(false));iq.disconnect();resources.clear();handshake=std::time::Instant::now();let fatal = fatal_error(&error);emit(BackendEvent::Status{online:false,error:fatal,text:if fatal{"Sign-in failed. Check credentials, TLS certificate and server configuration."}else{"Disconnected. Reconnecting…"}.into()});if fatal{break}},
                        None=>break,
                        Some(Event::Stanza(stanza))=>{
                            let root:Element=stanza.into();if iq.receive(&root,&owner){continue}let from=root.attr("from").unwrap_or("");
                            if root.is("message",NS){
                                if root.attr("type")==Some("error"){emit(BackendEvent::Failed{peer:bare_of(from),id:root.attr("id").unwrap_or("").into()});continue}
                                if let Some(r)=root.get_child("received","urn:xmpp:receipts"){emit(BackendEvent::Receipt{peer:bare_of(from),id:r.attr("id").unwrap_or("").into()})}
                                if let Some(wrapper)=root.get_child("result","urn:xmpp:mam:2"){
                                    if !(from.is_empty()||from==owner){continue}let query=wrapper.attr("queryid").unwrap_or("");let Some((expected,_))=mam.get(query)else{continue};let Some(forwarded)=wrapper.get_child("forwarded","urn:xmpp:forward:0")else{continue};let Some(inner)=forwarded.get_child("message",NS)else{continue};let stamp=forwarded.get_child("delay","urn:xmpp:delay").and_then(|d|d.attr("stamp")).unwrap_or("");if let Some((peer,_message,_))=normalize(inner,&owner,Some((wrapper.attr("id").unwrap_or(""),stamp))){if &peer==expected{deliver(&crypto,&emit,inner,&owner,Some((wrapper.attr("id").unwrap_or(""),stamp)),true,None)}}continue
                                }
                                let carbon=root.get_child("received","urn:xmpp:carbons:2").or_else(||root.get_child("sent","urn:xmpp:carbons:2"));
                                if let Some(wrapper)=carbon{if from!=owner{continue}if let Some(inner)=wrapper.get_child("forwarded","urn:xmpp:forward:0").and_then(|f|f.get_child("message",NS)){deliver(&crypto,&emit,inner,&owner,None,true,None);}continue}
                                if let Some((_peer,message,historical))=normalize(&root,&owner,None){let receipt_to=(!historical&&!message.own&&!message.wire_id.is_empty()&&root.get_child("request","urn:xmpp:receipts").is_some()).then(||from.to_string());deliver(&crypto,&emit,&root,&owner,None,historical,receipt_to);}
                                if root.get_child("delay","urn:xmpp:delay").is_none(){for name in ["composing","paused","active","inactive","gone"]{if root.get_child(name,"http://jabber.org/protocol/chatstates").is_some(){emit(BackendEvent::Typing{peer:bare_of(from),composing:name=="composing"})}}}
                            }else if root.is("presence",NS){let peer=bare_of(from);let kind=root.attr("type").unwrap_or("");if kind=="subscribe"{emit(BackendEvent::Request(peer));continue}let set=resources.entry(peer.clone()).or_default();if kind=="unavailable"{set.remove(from);}else if kind.is_empty(){set.insert(from.into());}emit(BackendEvent::Presence{peer,online:!set.is_empty()});
                            }else if root.is("iq",NS){let id=root.attr("id").unwrap_or("");let trusted=from.is_empty()||from==owner;
                                if trusted{if let Some(query)=root.get_child("query","jabber:iq:roster"){let contacts=query.children().filter(|c|c.is("item","jabber:iq:roster")).filter_map(|c|Some((c.attr("jid")?.into(),c.attr("name").unwrap_or("").into(),c.attr("subscription").unwrap_or("none").into()))).collect();emit(BackendEvent::Roster(contacts));if root.attr("type")==Some("set"){send(&mut client,Element::builder("iq",NS).attr("type".try_into().expect("static XML attribute"),"result").attr("id".try_into().expect("static XML attribute"),id).build()).await;}}}
                                if !trusted && mam.contains_key(id) { continue } if let Some((peer,_))=mam.remove(id){let fin=root.get_child("fin","urn:xmpp:mam:2");let before=fin.and_then(|f|f.get_child("set","http://jabber.org/protocol/rsm")).and_then(|s|s.get_child("first","http://jabber.org/protocol/rsm")).map(|f|f.text()).unwrap_or_default();let complete=fin.is_some_and(|f|matches!(f.attr("complete"),Some("true"|"1")))||before.is_empty();emit(BackendEvent::History{peer,before,complete,error:fin.is_none()});}
                                if root.attr("type")==Some("get")&&root.get_child("query","http://jabber.org/protocol/disco#info").is_some(){let mut query=el("query","http://jabber.org/protocol/disco#info");for ns in ["urn:xmpp:receipts",REPLY,SID,"http://jabber.org/protocol/chatstates",crate::message_actions::CORRECT,crate::message_actions::REACTIONS,crate::message_actions::RETRACT,crate::polls::NS,crate::forwarding::NS,crate::file_share::NS]{query.append_child(Element::builder("feature","http://jabber.org/protocol/disco#info").attr("var".try_into().expect("static XML attribute"),ns).build());}if crypto_ready{query.append_child(Element::builder("feature","http://jabber.org/protocol/disco#info").attr("var".try_into().unwrap(),"urn:xmpp:omemo:2").build());}send(&mut client,Element::builder("iq",NS).attr("to".try_into().expect("static XML attribute"),from).attr("type".try_into().expect("static XML attribute"),"result").attr("id".try_into().expect("static XML attribute"),id).append(query).build()).await;}
                            }
                        }
                    }},
                    _=timer.tick()=>{iq.expire();if !online&&handshake.elapsed()>Duration::from_secs(45){emit(BackendEvent::Status{online:false,error:true,text:"Connection timed out. Check the account settings.".into()});break}let expired:Vec<_>=mam.iter().filter(|(_,(_,start))|start.elapsed()>Duration::from_secs(20)).map(|(id,_)|id.clone()).collect();for id in expired{if let Some((peer,_))=mam.remove(&id){emit(BackendEvent::History{peer,before:String::new(),complete:false,error:true})}}}
                }}
                iq.disconnect();
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
fn deliver(
    crypto: &crate::crypto::service::Handle,
    emit: &impl Fn(BackendEvent),
    xml: &Element,
    owner: &str,
    archive: Option<(&str, &str)>,
    historical: bool,
    receipt_to: Option<String>,
) {
    if xml.get_child("encrypted", "urn:xmpp:omemo:2").is_some() {
        crypto.send(crate::crypto::service::Request::Incoming(
            crate::crypto::service::Frame {
                xml: xml.clone(),
                archive: archive.map(|(id, stamp)| (id.into(), stamp.into())),
                historical,
                receipt_to,
            },
        ));
    } else if let Some((peer, message, delayed)) = normalize(xml, owner, archive) {
        emit(BackendEvent::Message {
            peer,
            message,
            historical: historical || delayed,
            receipt_to,
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
