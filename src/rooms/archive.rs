//! XEP-0313 pages are scoped to the account archive or a joined room's own archive.
use super::*;
use crate::{model::new_id, protocol::BackendEvent};
use std::time::Instant;

pub const NS: &str = "urn:xmpp:mam:2";
const RSM: &str = "http://jabber.org/protocol/rsm";
const FORM: &str = "jabber:x:data";
const FORWARDED: &str = "urn:xmpp:forward:0";
const DELAY: &str = "urn:xmpp:delay";

pub struct Page {
    pub peer: String,
    pub room: bool,
    pub started: Instant,
    before: String,
}

pub struct ResultStanza<'a> {
    pub xml: &'a Element,
    pub uid: &'a str,
    pub stamp: &'a str,
}

impl Page {
    pub fn query(peer: String, room: bool, before: String) -> (String, Element, Self) {
        let id = new_id();
        let mut form = attr(el("x", FORM), "type", "submit");
        let mut form_type = attr(
            attr(el("field", FORM), "var", "FORM_TYPE"),
            "type",
            "hidden",
        );
        form_type.append_child(value("value", FORM, NS));
        form.append_child(form_type);
        if !room {
            let mut field = attr(el("field", FORM), "var", "with");
            field.append_child(value("value", FORM, &peer));
            form.append_child(field);
        }
        let mut set = el("set", RSM);
        set.append_child(value("max", RSM, "50"));
        set.append_child(value("before", RSM, &before));
        let mut query = attr(el("query", NS), "queryid", &id);
        query.append_child(form);
        query.append_child(set);
        let mut request = attr(attr(el("iq", CLIENT), "type", "set"), "id", &id);
        if room {
            request = attr(request, "to", &peer);
        }
        request.append_child(query);
        let page = Self {
            peer,
            room,
            started: Instant::now(),
            before,
        };
        (id, request, page)
    }

    pub fn accepts(&self, envelope: &Element, owner: &str) -> bool {
        let from = envelope.attr("from").unwrap_or("");
        addressed_to(envelope, owner)
            && if self.room {
                from == self.peer
            } else {
                from.is_empty() || from == owner
            }
    }

    pub fn result<'a>(&self, wrapper: &'a Element) -> Option<ResultStanza<'a>> {
        let forwarded = child(wrapper, "forwarded", FORWARDED).ok()??;
        let xml = child(forwarded, "message", CLIENT).ok()??;
        let uid = wrapper.attr("id").unwrap_or("");
        let stamp = child(forwarded, "delay", DELAY)
            .ok()?
            .and_then(|delay| delay.attr("stamp"))
            .unwrap_or("");
        if self.room && (!valid_uid(uid) || chrono::DateTime::parse_from_rfc3339(stamp).is_err()) {
            return None;
        }
        Some(ResultStanza { xml, uid, stamp })
    }

    pub fn finish(self, stanza: &Element) -> BackendEvent {
        let fin = child(stanza, "fin", NS).ok().flatten();
        let before = fin
            .and_then(|fin| child(fin, "set", RSM).ok().flatten())
            .and_then(|set| child(set, "first", RSM).ok().flatten())
            .filter(|first| first.children().next().is_none())
            .map(Element::text)
            .unwrap_or_default();
        let complete = fin.is_some_and(|f| matches!(f.attr("complete"), Some("true" | "1")))
            || before.is_empty();
        let error = stanza.attr("type") != Some("result")
            || fin.is_none()
            || (!before.is_empty()
                && (!valid_uid(&before) || (before == self.before && !complete)));
        BackendEvent::History {
            peer: self.peer,
            before,
            complete: complete && !error,
            error,
        }
    }
}

pub fn failed(peer: String) -> BackendEvent {
    BackendEvent::History {
        peer,
        before: String::new(),
        complete: false,
        error: true,
    }
}

pub fn valid_uid(uid: &str) -> bool {
    !uid.is_empty() && uid.len() <= 4096
}

fn value(name: &str, namespace: &str, value: &str) -> Element {
    Element::builder(name, namespace).append(value).build()
}
