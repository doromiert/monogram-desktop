//! XEP-0054 public profile fields, matching Monogram mobile's XmppProfile.
use crate::{iq, text_safety};
use tokio_xmpp::{jid::Jid, minidom::Element};
pub const NS: &str = "vcard-temp";
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub fields: Vec<(String, String)>,
}
fn field(card: &Element, path: &str) -> String {
    let mut element = card;
    for part in path.split('/') {
        let Some(child) = element.get_child(part, NS) else {
            return String::new();
        };
        element = child;
    }
    text_safety::visible(&element.text(), if path == "DESC" { 4096 } else { 512 })
}
pub fn parse(card: &Element) -> Result<Profile, &'static str> {
    if !card.is("vCard", NS) {
        return Err("Invalid profile response");
    }
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|v| *v <= 2 * 1024 * 1024)
                .ok_or_else(|| std::io::Error::other("Profile too large"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    card.write_to(&mut Budget(0))
        .map_err(|_| "Profile is too large")?;
    let mut fields = Vec::new();
    for (path, label) in [
        ("NICKNAME", "Nickname"),
        ("FN", "Full name"),
        ("DESC", "About"),
        ("URL", "Website"),
        ("BDAY", "Birthday"),
        ("ORG/ORGNAME", "Organization"),
        ("ORG/ORGUNIT", "Department"),
        ("TITLE", "Title"),
        ("ROLE", "Role"),
    ] {
        let text = field(card, path);
        if !text.is_empty() {
            fields.push((label.into(), text));
        }
    }
    for child in card.children().filter(|c| c.ns() == NS).take(64) {
        let pair = match child.name() {
            "EMAIL" => Some(("Email", field(child, "USERID"))),
            "TEL" => Some(("Phone", field(child, "NUMBER"))),
            "ADR" => Some((
                "Address",
                ["STREET", "LOCALITY", "REGION", "PCODE", "CTRY"]
                    .iter()
                    .map(|p| field(child, p))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
            _ => None,
        };
        if let Some((name, text)) = pair.filter(|(_, s)| !s.is_empty()) {
            fields.push((name.into(), text));
        }
    }
    Ok(Profile { fields })
}
pub async fn fetch(client: &iq::Client, peer: &str) -> Result<Profile, &'static str> {
    let jid = peer.parse::<Jid>().map_err(|_| "Invalid profile address")?;
    if jid.node().is_none() || jid.to_string() != peer {
        return Err("Invalid profile address");
    }
    match client
        .request(peer, iq::Kind::Get, Element::builder("vCard", NS).build())
        .await
    {
        Ok(Some(card)) => parse(&card),
        Ok(None) => Ok(Profile::default()),
        Err(iq::Error::Rejected { condition, .. })
            if matches!(
                condition.as_str(),
                "item-not-found" | "service-unavailable" | "feature-not-implemented" | "forbidden"
            ) =>
        {
            Ok(Profile::default())
        }
        Err(_) => Err("Could not load this profile. Check your connection and try again."),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mobile_vcard_fields_are_bounded_plain_text_and_keep_repeated_contacts() {
        let xml:Element="<vCard xmlns='vcard-temp'><NICKNAME>Alice</NICKNAME><FN>Alice &amp; Bob</FN><DESC>hello &lt;b&gt;world&lt;/b&gt;</DESC><EMAIL><USERID>a@example.org</USERID></EMAIL><EMAIL><USERID>b@example.org</USERID></EMAIL><PHOTO><EXTVAL>https://untrusted.invalid/never-fetch</EXTVAL></PHOTO></vCard>".parse().unwrap();
        let card = parse(&xml).unwrap();
        assert_eq!(card.fields.len(), 5);
        assert_eq!(card.fields[2].1, "hello <b>world</b>");
        assert!(card.fields.iter().all(|(_, v)| !v.contains("never-fetch")));
        let xml: Element = format!(
            "<vCard xmlns='vcard-temp'><DESC>{}</DESC></vCard>",
            "x".repeat(8000)
        )
        .parse()
        .unwrap();
        assert_eq!(parse(&xml).unwrap().fields[0].1.chars().count(), 4096);
    }
}
