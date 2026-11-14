//! XEP-0085 content negotiation shared with the Android chat-state implementation.
use tokio_xmpp::minidom::Element;
pub const NS:&str="http://jabber.org/protocol/chatstates";
pub fn decorate(root:&mut Element) {
    if matches!(root.attr("type"),Some("chat")) && (root.get_child("body","jabber:client").is_some() || root.get_child("encrypted","eu.siacs.conversations.axolotl").is_some()) {
        root.append_child(Element::builder("active",NS).build());
    }
}
pub fn incoming(root:&Element)->Option<bool> {
    if !matches!(root.attr("type"),Some("chat")) || root.children().any(|c|matches!(c.ns().as_str(),"urn:xmpp:delay"|"jabber:x:delay"|"urn:xmpp:forward:0"|"urn:xmpp:carbons:2"|"urn:xmpp:mam:2")) {return None;}
    let mut states=root.children().filter(|c|c.ns()==NS);let state=states.next()?;
    if states.next().is_some() || !state.attrs().is_empty() || state.children().next().is_some() || !state.text().trim().is_empty() {return None;}
    match state.name(){"composing"=>Some(true),"active"|"paused"|"inactive"|"gone"=>Some(false),_=>None}
}
#[cfg(test)] mod tests {
 use super::*;
 #[test]fn live_chat_states_and_content_negotiation(){
  let mut xml:Element="<message xmlns='jabber:client' type='chat'><body>hi</body></message>".parse().unwrap();decorate(&mut xml);assert_eq!(incoming(&xml),Some(false));
  for (state,expected) in [("composing",true),("paused",false)]{let xml:Element=format!("<message xmlns='jabber:client' type='chat'><{state} xmlns='{NS}'/></message>").parse().unwrap();assert_eq!(incoming(&xml),Some(expected));}
  let xml:Element=format!("<message xmlns='jabber:client' type='chat'><composing xmlns='{NS}'/><delay xmlns='urn:xmpp:delay'/></message>").parse().unwrap();assert_eq!(incoming(&xml),None);
 }
}
