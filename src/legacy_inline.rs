//! Proposed Monogram authenticated Legacy OMEMO inline-emoji profile.
//! Advertise/negotiate before sending. Never interpret unauthenticated outside BoB.
use tokio_xmpp::minidom::Element;
pub const NS:&str="urn:monogram:legacy-inline:0";
pub const PREFIX:&str="urn:monogram:legacy-inline:0\n";
const MAX:usize=256*1024;
pub fn encode(clear:&Element)->Option<String>{
 let body=clear.get_child("body","jabber:client")?.text();
 crate::packs::InlineEmoji::from_message(clear,&body)?;
 let mut content=Element::builder("message","jabber:client").build();
 for c in clear.children().filter(|c|c.is("body","jabber:client")||c.is("html","http://jabber.org/protocol/xhtml-im")||c.is("data","urn:xmpp:bob")){content.append_child(c.clone());}
 let mut bytes=Vec::new();content.write_to(&mut bytes).ok()?;
 let wire=format!("{PREFIX}{}",String::from_utf8(bytes).ok()?);
 (wire.len()<=MAX).then_some(wire)
}
fn allowed(c:&Element)->bool{c.is("body","jabber:client")||c.is("html","http://jabber.org/protocol/xhtml-im")||c.is("data","urn:xmpp:bob")||c.is("markup",crate::formatting::NS)||c.is("file-sharing",crate::file_share::NS)}
pub fn decode(body:&str)->Result<Option<Element>, &'static str>{
 let Some(xml)=body.strip_prefix(PREFIX) else{return Ok(None);};
 if body.len()>MAX || xml.contains("<!"){return Err("Invalid authenticated inline emoji content");}
 let content:Element=xml.parse().map_err(|_|"Invalid authenticated inline emoji content")?;
 if !content.is("message","jabber:client") || !content.attrs().is_empty() || content.children().any(|c|!allowed(c)) || content.children().filter(|c|c.is("body","jabber:client")).count()!=1{return Err("Invalid authenticated inline emoji content");}
 let text=content.get_child("body","jabber:client").unwrap().text();
 crate::packs::InlineEmoji::from_message(&content,&text).ok_or("Invalid authenticated inline emoji assets")?;
 Ok(Some(content))
}
#[cfg(test)]mod tests{
 use super::*;
 #[test]fn real_inline_content_round_trips(){
  use std::sync::Arc;use base64::Engine;
  let bytes=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=").unwrap();
  let asset=crate::packs::InlineAsset{cid:format!("sha-256+{}@bob.xmpp.org",crate::crypto::lifecycle::hex(&openssl::sha::sha256(&bytes))),mime:"image/png".into(),bytes:Arc::new(bytes)};
  let inline=crate::packs::InlineEmoji::new("🙂".into(),vec![crate::packs::InlineRange{start:0,end:1,asset}]).unwrap();
  let mut clear=Element::builder("message","jabber:client").append(Element::builder("body","jabber:client").append("🙂").build()).build();inline.add_to(&mut clear).unwrap();
  let wire=encode(&clear).unwrap();assert!(!wire.contains(crate::packs::emoji_markup::NS));assert!(!wire.contains(crate::file_share::NS));assert_eq!(crate::packs::InlineEmoji::from_message(&decode(&wire).unwrap().unwrap(),"🙂"),Some(inline));
 }
 #[test]fn reads_mobile_generated_payload(){let wire=include_str!("../tests/fixtures/legacy-inline/mobile.txt");let clear=decode(wire).unwrap().unwrap();let inline=crate::packs::InlineEmoji::from_message(&clear,"🙂🙂").unwrap();assert_eq!(inline.ranges.len(),2);assert_eq!(inline.ranges[0].asset.mime,"image/png");}
 #[test]fn rejects_untrusted_or_malformed_envelopes(){assert!(decode("ordinary body").unwrap().is_none());assert!(decode(&format!("{PREFIX}<message xmlns='jabber:client'><body>🙂</body></message>")).is_err());assert!(decode(&format!("{PREFIX}<!DOCTYPE x><message/>" )).is_err());}
}
