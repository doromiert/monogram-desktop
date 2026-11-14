//! XEP-0514 spans and XEP-0447 media, with hash-verified, in-stanza BoB sources.
use crate::packs::{Error, InlineAsset, InlineEmoji, InlineRange};
use std::collections::{HashMap, HashSet};
use tokio_xmpp::minidom::Element;
use base64::{Engine, engine::general_purpose::STANDARD};
pub const NS: &str = "urn:xmpp:markup:emoji:0";
const MARKUP: &str = "urn:xmpp:markup:0";
const SFS: &str = "urn:xmpp:sfs:0";
const FILE: &str = "urn:xmpp:file:metadata:0";
const HASH: &str = "urn:xmpp:hashes:2";
const URL: &str = "http://jabber.org/protocol/url-data";
fn digest(asset: &InlineAsset) -> Element {
    Element::builder("hash", HASH).attr("algo".try_into().unwrap(), "sha-256")
        .append(STANDARD.encode(openssl::sha::sha256(&asset.bytes))).build()
}
pub fn append(message: &mut Element, model: &InlineEmoji) {
    let mut markup = message.get_child("markup", MARKUP).cloned().unwrap_or_else(|| Element::builder("markup", MARKUP).build());
    let mut seen = HashSet::new();
    for range in &model.ranges {
        markup.append_child(Element::builder("span", MARKUP)
            .attr("start".try_into().unwrap(), range.start.to_string()).attr("end".try_into().unwrap(), range.end.to_string())
            .append(Element::builder("emoji", NS).append(digest(&range.asset)).build()).build());
        if seen.insert(&range.asset.cid) {
            let file = Element::builder("file", FILE)
                .append(Element::builder("media-type", FILE).append(range.asset.mime.as_str()).build())
                .append(Element::builder("size", FILE).append(range.asset.bytes.len().to_string()).build())
                .append(digest(&range.asset)).build();
            message.append_child(Element::builder("file-sharing", SFS).attr("disposition".try_into().unwrap(),"inline")
                .append(file).append(Element::builder("sources", SFS).append(Element::builder("url-data", URL)
                    .attr("target".try_into().unwrap(),format!("cid:{}",range.asset.cid)).build()).build()).build());
        }
    }
    if let Some(existing) = message.get_child_mut("markup", MARKUP) { *existing = markup; } else { message.append_child(markup); }
}
/// Only resolves authenticated, already supplied bytes. Never fetches an arbitrary URL.
pub fn read(message: &Element, body: &str, assets: &HashMap<String, InlineAsset>) -> Option<Result<InlineEmoji, Error>> {
    let markups: Vec<_> = message.children().filter(|c|c.is("markup",MARKUP)).collect();
    if !markups.iter().any(|m|m.children().any(|s|s.get_child("emoji",NS).is_some())) {return None;}
    Some((|| {
        if markups.len()!=1 || markups[0].children().count()>256 || message.children().filter(|c|c.is("file-sharing",SFS)).count()>16 {return Err(Error("Too much emoji markup."));}
        let mut media = HashMap::new();
        for sharing in message.children().filter(|c|c.is("file-sharing",SFS)).take(17) {
            let Some(file)=sharing.get_child("file",FILE) else{continue;};
            let Some(sources)=sharing.get_child("sources",SFS) else{continue;};
            for source in sources.children().filter(|c|c.is("url-data",URL)) {
                let Some(asset)=source.attr("target").and_then(|s|s.strip_prefix("cid:")).and_then(|s|assets.get(s)) else{continue;};
                if file.get_child("media-type",FILE).map(|e|e.text()).as_deref()!=Some(&asset.mime)
                    || file.get_child("size",FILE).and_then(|e|e.text().parse::<usize>().ok())!=Some(asset.bytes.len()) {continue;}
                for h in file.children().filter(|c|c.is("hash",HASH)&&c.attr("algo")==Some("sha-256")) {
                    let expected=STANDARD.encode(openssl::sha::sha256(&asset.bytes));
                    if h.text()==expected {media.insert(expected,asset.clone());}
                }
            }
        }
        let mut ranges=Vec::new();
        for span in markups[0].children().take(257) {
            let Some(emoji)=span.get_child("emoji",NS) else{continue;};
            if !span.is("span",MARKUP)||ranges.len()>=64 {return Err(Error("Invalid emoji span."));}
            let start=span.attr("start").and_then(|s|s.parse().ok()).ok_or(Error("Invalid emoji start."))?;
            let end=span.attr("end").and_then(|s|s.parse().ok()).ok_or(Error("Invalid emoji end."))?;
            let asset=emoji.children().filter(|c|c.is("hash",HASH)&&c.attr("algo")==Some("sha-256"))
                .find_map(|h|media.get(&h.text())).cloned().ok_or(Error("Missing verified emoji media."))?;
            ranges.push(InlineRange{start,end,asset});
        }
        if ranges.is_empty(){return Err(Error("Missing emoji spans."));}
        InlineEmoji::new(body.into(),ranges)
    })())
}
#[cfg(test)]mod tests {
 use super::*;
 #[test]fn standard_spans_survive_without_xhtml_and_verify_both_hashes(){
  let asset=InlineAsset::new("image/png".into(),STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=").unwrap()).unwrap();
  let model=InlineEmoji::new("😀 :test: end".into(),vec![InlineRange{start:2,end:8,asset}]).unwrap();
  let mut message=Element::builder("message","jabber:client").append(Element::builder("body","jabber:client").append(model.body.as_str()).build()).build();
  model.add_to(&mut message).unwrap();
  let mut standard=Element::builder("message","jabber:client").build();for c in message.children().filter(|c|!c.is("html","http://jabber.org/protocol/xhtml-im")){standard.append_child(c.clone());}
  assert_eq!(InlineEmoji::from_message(&standard,&model.body),Some(model.clone()));
  let mut raw=Vec::new();standard.write_to(&mut raw).unwrap();let xml=String::from_utf8(raw).unwrap().replace("sha-256\"","sha-512\"").replace("sha-256'","sha-512'");
  let corrupt:Element=xml.parse().unwrap();assert!(InlineEmoji::from_message(&corrupt,&model.body).is_none());
 }
}
