//! Live XEP-0085 chat states; replayed states must never change the header.
use std::time::Duration;
use tokio_xmpp::minidom::Element;
pub const NS: &str = "http://jabber.org/protocol/chatstates";
// Match mobile: a sender must not repeat composing until its state changes.
pub const TIMEOUT: Duration = Duration::from_secs(120);

pub fn decorate(root: &mut Element) {
    if matches!(root.attr("type"), Some("chat" | "groupchat"))
        && (root.get_child("body", "jabber:client").is_some()
            || root.get_child("encrypted", "eu.siacs.conversations.axolotl").is_some())
    {
        root.append_child(Element::builder("active", NS).build());
    }
}

pub fn incoming(root: &Element) -> Option<bool> {
    if !root.is("message", "jabber:client")
        || !matches!(root.attr("type"), None | Some("normal" | "chat" | "groupchat"))
        || root.children().any(|c| matches!(c.ns().as_str(),
            "urn:xmpp:delay" | "jabber:x:delay" | "urn:xmpp:forward:0" |
            "urn:xmpp:carbons:2" | "urn:xmpp:mam:2"))
    {
        return None;
    }
    let mut states = root.children().filter(|c| c.ns() == NS);
    let Some(state) = states.next() else {
        // A live content message ends this sender's composing state even if the
        // other client omits the recommended active element.
        return (root.get_child("body", "jabber:client").is_some()
            || root.children().any(|c| c.name() == "encrypted")).then_some(false);
    };
    if states.next().is_some() || !state.attrs().is_empty()
        || state.children().next().is_some() || !state.text().trim().is_empty()
    {
        return None;
    }
    match state.name() {
        "composing" => Some(true),
        "active" | "paused" | "inactive" => Some(false),
        "gone" if root.attr("type") != Some("groupchat") => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_chat_states_and_content_negotiation() {
        for kind in ["type='chat'", "type='groupchat'", "type='normal'", ""] {
            for (state, expected) in [("composing", true), ("paused", false), ("active", false)] {
                let xml: Element = format!("<message xmlns='jabber:client' {kind}><{state} xmlns='{NS}'/></message>").parse().unwrap();
                assert_eq!(incoming(&xml), Some(expected));
            }
        }
        let mut xml: Element = "<message xmlns='jabber:client' type='groupchat'><body>hi</body></message>".parse().unwrap();
        decorate(&mut xml);
        assert_eq!(incoming(&xml), Some(false));
        assert!(xml.get_child("active", NS).is_some());
        let xml: Element = "<message xmlns='jabber:client' type='chat'><body>hi</body></message>".parse().unwrap();
        assert_eq!(incoming(&xml), Some(false));
        for extra in ["<delay xmlns='urn:xmpp:delay'/>", "<x xmlns='jabber:x:delay'/>", "<received xmlns='urn:xmpp:carbons:2'/>", "<result xmlns='urn:xmpp:mam:2'/>", "<forwarded xmlns='urn:xmpp:forward:0'/>"] {
            let xml: Element = format!("<message xmlns='jabber:client' type='chat'><composing xmlns='{NS}'/>{extra}</message>").parse().unwrap();
            assert_eq!(incoming(&xml), None);
        }
        for content in [format!("<composing xmlns='{NS}'/><paused xmlns='{NS}'/>"), format!("<composing xmlns='{NS}' bad='1'/>"), format!("<composing xmlns='{NS}'>bad</composing>"), "<received xmlns='urn:xmpp:receipts' id='one'/>".into()] {
            let xml: Element = format!("<message xmlns='jabber:client'>{content}</message>").parse().unwrap();
            assert_eq!(incoming(&xml), None);
        }
    }
}
