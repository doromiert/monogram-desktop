//! XEP-0317 hats: extra titles a room gives its occupants ("Host", "Teacher's Assistant").
//!
//! Hats arrive in occupant presence, inside `<hats xmlns='urn:xmpp:hats:0'>`. Everything in them
//! comes from the network, so it is cut down to bounded, printable values here and nowhere else.
//! Whether to believe them at all is decided by the room: only a room that advertises the
//! `urn:xmpp:hats:0` feature in its service discovery is trusted to have set them itself rather
//! than reflecting whatever an occupant put in their own presence.
use super::Element;

pub const NS: &str = "urn:xmpp:hats:0";
/// The service discovery feature a room advertises when it supports hats.
pub const FEATURE: &str = NS;
/// More hats than this on one occupant are ignored; nobody wears that many.
pub const MAX_HATS: usize = 8;
const MAX_TITLE_CHARS: usize = 40;
const MAX_URI_BYTES: usize = 512;
const MAX_HUE_TEXT: usize = 32;

/// One hat an occupant wears.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hat {
    /// Name shown to people, already cleaned of control and direction-changing characters.
    pub title: String,
    /// The hat's unique identity within the room.
    pub uri: String,
    /// Hue angle in thousandths of a degree, `0..360_000`. Kept as an integer so a participant
    /// stays comparable.
    hue_milli: Option<u32>,
}

impl Hat {
    pub fn new(title: &str, uri: &str, hue: Option<f64>) -> Option<Self> {
        let title = crate::text_safety::visible(title, MAX_TITLE_CHARS);
        let uri = uri.trim();
        if title.is_empty() || uri.is_empty() || uri.len() > MAX_URI_BYTES {
            return None;
        }
        Some(Self { title, uri: uri.into(), hue_milli: hue.and_then(milli) })
    }
    /// The hue angle in degrees, `0.0..360.0`, when the room gave a usable one.
    pub fn hue(&self) -> Option<f64> {
        self.hue_milli.map(|m| f64::from(m) / 1000.0)
    }
}

/// A hue as the wire writes it (`"327.255249"`): finite numbers only, wrapped into one turn.
fn milli(degrees: f64) -> Option<u32> {
    degrees
        .is_finite()
        .then(|| ((degrees.rem_euclid(360.0) * 1000.0).round() as u32) % 360_000)
}

pub fn parse_hue(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_HUE_TEXT {
        return None;
    }
    // Plain decimal only: no "inf", "nan", exponents or hex floats from the network.
    if !text.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+')) {
        return None;
    }
    text.parse::<f64>().ok().filter(|hue| hue.is_finite())
}

/// The hats in an occupant's presence, in the order given, without repeats. An invalid hat is
/// skipped on its own; it never discards the valid ones beside it.
pub fn parse(presence: &Element) -> Vec<Hat> {
    let Some(container) = presence.get_child("hats", NS) else {
        return Vec::new();
    };
    let mut hats: Vec<Hat> = Vec::new();
    for element in container.children().filter(|child| child.is("hat", NS)) {
        if hats.len() >= MAX_HATS {
            break;
        }
        let hue = element.attr("hue").and_then(parse_hue);
        let Some(hat) =
            Hat::new(element.attr("title").unwrap_or(""), element.attr("uri").unwrap_or(""), hue)
        else {
            continue;
        };
        if hats.iter().all(|seen| seen.uri != hat.uri) {
            hats.push(hat);
        }
    }
    hats
}

/// Whether a disco#info `query` advertises hats.
pub fn advertised(info: &Element) -> bool {
    info.children().any(|child| {
        child.is("feature", "http://jabber.org/protocol/disco#info") && child.attr("var") == Some(FEATURE)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presence(inner: &str) -> Element {
        format!("<presence xmlns='jabber:client' from='room@muc.example/Harry'>{inner}</presence>")
            .parse()
            .unwrap()
    }

    #[test]
    fn the_specs_example_parses_in_order_with_hues() {
        let hats = parse(&presence(
            "<hats xmlns='urn:xmpp:hats:0'>\
               <hat title='Host' uri='http://schemas.example.com/hats#host' hue='327.255249' xml:lang='en-us'>\
                 <badge xmlns='urn:example:badges' level='3'/></hat>\
               <hat title='Presenter' uri='http://schemas.example.com/hats#presenter' hue='171.430664'/>\
             </hats>",
        ));
        assert_eq!(hats.len(), 2);
        assert_eq!((hats[0].title.as_str(), hats[1].title.as_str()), ("Host", "Presenter"));
        assert_eq!(hats[0].hue(), Some(327.255));
        assert_eq!(hats[1].hue(), Some(171.431));
    }

    #[test]
    fn presence_without_hats_or_in_another_namespace_has_none() {
        assert!(parse(&presence("")).is_empty());
        assert!(parse(&presence("<hats xmlns='urn:example:other'><hat title='A' uri='u'/></hats>")).is_empty());
        assert!(parse(&presence("<hats xmlns='urn:xmpp:hats:0'/>")).is_empty());
        assert!(parse(&presence("<hat xmlns='urn:xmpp:hats:0' title='A' uri='u'/>")).is_empty(), "no container");
        assert!(parse(&presence("<hats xmlns='urn:xmpp:hats:0'><hat xmlns='urn:example:x' title='A' uri='u'/></hats>")).is_empty());
    }

    #[test]
    fn a_hat_needs_a_title_and_a_uri_and_a_bad_one_does_not_hide_the_good() {
        let hats = parse(&presence(
            "<hats xmlns='urn:xmpp:hats:0'>\
               <hat uri='u1'/><hat title='No Uri'/><hat title='' uri='u2'/><hat title='   ' uri='u3'/>\
               <hat title='\u{202e}\u{200b}' uri='u4'/><hat title='Good' uri='u5'/>\
             </hats>",
        ));
        assert_eq!(hats.iter().map(|h| h.title.as_str()).collect::<Vec<_>>(), ["Good"]);
    }

    #[test]
    fn titles_are_cleaned_and_cut_and_uris_are_bounded() {
        let long = "x".repeat(500);
        let hats = parse(&presence(&format!(
            "<hats xmlns='urn:xmpp:hats:0'>\
               <hat title='Mod\u{202e}erator\u{200b}' uri='a'/>\
               <hat title='{long}' uri='b'/>\
               <hat title='  spaced \n out  ' uri='c'/>\
               <hat title='Fine' uri='{}'/>\
             </hats>",
            "u".repeat(513)
        )));
        assert_eq!(hats[0].title, "Moderator");
        assert_eq!(hats[1].title.chars().count(), 40);
        assert!(hats[1].title.ends_with('…'));
        assert_eq!(hats[2].title, "spaced out");
        assert_eq!(hats.len(), 3, "a 513-byte uri is refused");
        assert!(Hat::new("Fine", &"u".repeat(512), None).is_some());
    }

    #[test]
    fn the_same_hat_twice_and_too_many_hats_are_limited() {
        let hats = parse(&presence(
            "<hats xmlns='urn:xmpp:hats:0'><hat title='A' uri='same'/><hat title='B' uri='same'/></hats>",
        ));
        assert_eq!(hats.len(), 1);
        assert_eq!(hats[0].title, "A", "the first wins");
        let many: String = (0..50).map(|i| format!("<hat title='H{i}' uri='u{i}'/>")).collect();
        let hats = parse(&presence(&format!("<hats xmlns='urn:xmpp:hats:0'>{many}</hats>")));
        assert_eq!(hats.len(), MAX_HATS);
        assert_eq!(hats[MAX_HATS - 1].title, "H7");
        // Only the first container counts.
        let two = parse(&presence(
            "<hats xmlns='urn:xmpp:hats:0'><hat title='A' uri='a'/></hats>\
             <hats xmlns='urn:xmpp:hats:0'><hat title='B' uri='b'/></hats>",
        ));
        assert_eq!(two.len(), 1);
    }

    #[test]
    fn a_hue_is_a_finite_decimal_wrapped_into_one_turn() {
        for (text, want) in [
            ("0", Some(0.0)),
            ("327.255249", Some(327.255)),
            ("359.9996", Some(0.0)),
            ("360", Some(0.0)),
            ("720.5", Some(0.5)),
            ("-90", Some(270.0)),
            ("+120", Some(120.0)),
            ("  45.5  ", Some(45.5)),
            (".5", Some(0.5)),
        ] {
            let got = parse_hue(text).and_then(milli).map(|m| f64::from(m) / 1000.0);
            assert_eq!(got, want, "{text:?}");
        }
        for bad in ["", " ", "red", "NaN", "nan", "inf", "-inf", "1e3", "0x10", "12deg", "1,5", "--1", "1.2.3", "٣٢"] {
            assert_eq!(parse_hue(bad), None, "{bad:?}");
        }
        assert_eq!(parse_hue(&"1".repeat(33)), None, "over the length cap");
        assert_eq!(parse_hue("1e999"), None);
        assert!(parse_hue(&"9".repeat(32)).and_then(milli).is_some_and(|m| m < 360_000), "huge but finite still wraps");
    }

    #[test]
    fn a_bad_hue_keeps_the_hat_but_drops_the_colour() {
        let hats = parse(&presence(
            "<hats xmlns='urn:xmpp:hats:0'>\
               <hat title='A' uri='a' hue='red'/><hat title='B' uri='b' hue='NaN'/><hat title='C' uri='c'/>\
             </hats>",
        ));
        assert_eq!(hats.len(), 3);
        assert!(hats.iter().all(|hat| hat.hue().is_none()));
    }

    #[test]
    fn support_is_read_from_the_disco_features() {
        let info = |inner: &str| -> Element {
            format!("<query xmlns='http://jabber.org/protocol/disco#info'>{inner}</query>").parse().unwrap()
        };
        assert!(advertised(&info("<feature var='urn:xmpp:hats:0'/>")));
        assert!(advertised(&info("<identity category='conference' type='text'/><feature var='x'/><feature var='urn:xmpp:hats:0'/>")));
        assert!(!advertised(&info("")));
        assert!(!advertised(&info("<feature var='urn:xmpp:hats:1'/>")));
        assert!(!advertised(&info("<feature var='URN:XMPP:HATS:0'/>")));
        assert!(!advertised(&info("<identity var='urn:xmpp:hats:0'/>")));
    }
}
