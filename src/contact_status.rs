//! A contact's availability as their presence states it: `<show/>` (RFC 6121) and, when sent,
//! XEP-0319 "Last User Interaction in Presence" (`<idle since='…'/>`).
//!
//! `show` and idle time are separate facts: the XEP says idle time may accompany any `show`
//! value, so a contact can be available and idle at once. The status light follows `show`; how
//! long they have been idle is added to the text.
//!
//! The light is grey when there is nothing trustworthy to say: the contact's presence is not
//! shared with us (no subscription, or a refused one), or they are offline.
use chrono::{DateTime, Duration, Utc};
use tokio_xmpp::minidom::Element;

pub const IDLE_NS: &str = "urn:xmpp:idle:1";
/// A peer's clock may run a little ahead of ours; more than this and the timestamp is dropped.
const CLOCK_SKEW_SECONDS: i64 = 300;
/// An idle time older than this is nonsense for a connected client.
const MAX_IDLE_DAYS: i64 = 365;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Show {
    /// Available, or "chat".
    Available,
    /// "away" or "xa".
    Away,
    /// "dnd".
    DoNotDisturb,
}

/// What one connected device of a contact said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detail {
    pub show: Show,
    /// When they last used the device, if the presence said (XEP-0319).
    pub idle_since: Option<DateTime<Utc>>,
}

/// The status circle's colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    Green,
    Yellow,
    Red,
    Grey,
}

fn show(presence: &Element) -> Show {
    let text = presence
        .children()
        .find(|child| child.name() == "show")
        .map(|show| show.text().trim().to_ascii_lowercase());
    match text.as_deref() {
        Some("away" | "xa") => Show::Away,
        Some("dnd") => Show::DoNotDisturb,
        // "chat", nothing, and anything this client does not know are all "available".
        _ => Show::Available,
    }
}

/// XEP-0082 DateTime: `2018-01-01T12:00:00Z`, with optional fractional seconds or an offset.
pub fn parse_since(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if text.is_empty() || text.len() > 40 {
        return None;
    }
    // XEP-0082 writes the date and time with a "T"; RFC 3339 parsers also take a space.
    if text.as_bytes().get(10) != Some(&b'T') {
        return None;
    }
    let since = DateTime::parse_from_rfc3339(text).ok()?.with_timezone(&Utc);
    if since > now + Duration::try_seconds(CLOCK_SKEW_SECONDS)? {
        return None;
    }
    if since < now - Duration::try_days(MAX_IDLE_DAYS)? {
        return None;
    }
    // A slightly fast clock must not read as "idle from the future".
    Some(since.min(now))
}

/// What an available `<presence/>` says. Only the first `<idle/>` counts.
pub fn parse(presence: &Element, now: DateTime<Utc>) -> Detail {
    let idle_since = presence
        .children()
        .find(|child| child.is("idle", IDLE_NS))
        .and_then(|idle| idle.attr("since"))
        .and_then(|since| parse_since(since, now));
    Detail { show: show(presence), idle_since }
}

/// The detail to show for a contact who has several devices connected: the most reachable one
/// (available before away before do-not-disturb), and among equals the one used most recently.
pub fn best(details: impl IntoIterator<Item = Detail>) -> Option<Detail> {
    details.into_iter().min_by_key(|detail| {
        (
            detail.show,
            detail.idle_since.is_some(),
            std::cmp::Reverse(detail.idle_since),
        )
    })
}

/// The circle for a contact. `visible`: their presence is shared with us. `online`: at least one
/// device is connected. `detail` is only consulted when both are true.
pub fn light(visible: bool, online: bool, detail: Option<Detail>) -> Light {
    if !visible || !online {
        return Light::Grey;
    }
    match detail.map(|detail| detail.show) {
        Some(Show::Away) => Light::Yellow,
        Some(Show::DoNotDisturb) => Light::Red,
        // Online with no detail yet is simply online.
        Some(Show::Available) | None => Light::Green,
    }
}

impl Light {
    /// What the colour means, for tooltips and assistive technology: colour alone never carries it.
    pub fn meaning(self) -> &'static str {
        match self {
            Light::Green => "Online",
            Light::Yellow => "Away",
            Light::Red => "Do not disturb",
            Light::Grey => "Status unknown",
        }
    }
}

/// "Online", "Away", "Do not disturb" with, when known, how long they have been idle:
/// `Away · idle for 12 minutes · since Sat July 4 2015 23:25`.
pub fn describe(light: Light, detail: Option<Detail>, now: DateTime<Utc>) -> String {
    let base = match (light, detail) {
        (Light::Grey, _) => return light.meaning().into(),
        _ => light.meaning(),
    };
    match detail.and_then(|detail| detail.idle_since) {
        Some(since) if since <= now - Duration::try_seconds(60).unwrap_or_default() => format!(
            "{base} · idle for {} · since {}",
            crate::last_activity::elapsed(&since, &now),
            crate::last_activity::absolute(&since.with_timezone(&chrono::Local)),
        ),
        _ => base.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap()
    }
    fn presence(inner: &str) -> Element {
        format!("<presence xmlns='jabber:client' from='bob@example.org/phone'>{inner}</presence>").parse().unwrap()
    }
    fn detail(show: Show, minutes_ago: Option<i64>) -> Detail {
        Detail { show, idle_since: minutes_ago.map(|m| now() - Duration::try_minutes(m).unwrap()) }
    }

    #[test]
    fn show_values_map_to_three_states_and_unknown_means_available() {
        for (inner, want) in [
            ("", Show::Available),
            ("<show>chat</show>", Show::Available),
            ("<show>away</show>", Show::Away),
            ("<show>xa</show>", Show::Away),
            ("<show>dnd</show>", Show::DoNotDisturb),
            ("<show> DND </show>", Show::DoNotDisturb),
            ("<show>Away</show>", Show::Away),
            ("<show></show>", Show::Available),
            ("<show>sleeping</show>", Show::Available),
            ("<show>away</show><show>dnd</show>", Show::Away),
            ("<status>dnd</status>", Show::Available),
        ] {
            assert_eq!(parse(&presence(inner), now()).show, want, "{inner}");
        }
    }

    #[test]
    fn the_idle_element_gives_the_moment_of_last_interaction() {
        let d = parse(&presence("<show>away</show><idle xmlns='urn:xmpp:idle:1' since='2025-06-15T11:48:00Z'/>"), now());
        assert_eq!(d.show, Show::Away);
        assert_eq!(d.idle_since, Some(now() - Duration::try_minutes(12).unwrap()));
        // Idle time goes with any show value, including plain availability.
        let d = parse(&presence("<idle xmlns='urn:xmpp:idle:1' since='2025-06-15T11:00:00Z'/>"), now());
        assert_eq!((d.show, d.idle_since.is_some()), (Show::Available, true));
        assert_eq!(parse(&presence("<show>dnd</show>"), now()).idle_since, None, "coming back omits it");
    }

    #[test]
    fn timestamps_follow_xep_0082_and_bad_ones_are_dropped() {
        let n = now();
        let good = Some(Utc.with_ymd_and_hms(2025, 6, 15, 11, 0, 0).unwrap());
        for text in ["2025-06-15T11:00:00Z", "2025-06-15T11:00:00.000Z", "2025-06-15T13:00:00+02:00", "2025-06-15T06:00:00-05:00", "  2025-06-15T11:00:00Z  "] {
            assert_eq!(parse_since(text, n), good, "{text}");
        }
        assert!(parse_since("2025-06-15T11:00:00.5Z", n).is_some());
        for bad in ["", " ", "yesterday", "2025-06-15", "11:00:00", "2025-06-15 11:00:00Z", "2025-13-45T11:00:00Z", "1750000000", "2025-06-15T11:00:00", &"2".repeat(41)] {
            assert_eq!(parse_since(bad, n), None, "{bad:?}");
        }
        // Further ahead than clock skew, or older than a year: not believed.
        assert_eq!(parse_since("2025-06-15T12:06:00Z", n), None);
        assert_eq!(parse_since("2024-06-14T12:00:00Z", n), None);
        assert!(parse_since("2024-06-16T12:00:00Z", n).is_some());
        // A slightly fast clock is clamped to now.
        assert_eq!(parse_since("2025-06-15T12:03:00Z", n), Some(n));
        // A missing or malformed attribute keeps the presence and drops only the idle time.
        let d = parse(&presence("<show>away</show><idle xmlns='urn:xmpp:idle:1'/>"), n);
        assert_eq!((d.show, d.idle_since), (Show::Away, None));
        let d = parse(&presence("<idle xmlns='urn:example:other' since='2025-06-15T11:00:00Z'/>"), n);
        assert_eq!(d.idle_since, None, "wrong namespace");
    }

    #[test]
    fn several_devices_show_the_most_reachable_and_among_equals_the_most_recently_used() {
        assert_eq!(best([]), None);
        let (away, dnd, active) = (detail(Show::Away, Some(30)), detail(Show::DoNotDisturb, None), detail(Show::Available, None));
        assert_eq!(best([dnd, away, active]), Some(active));
        assert_eq!(best([dnd, away]), Some(away), "away is more reachable than do-not-disturb");
        assert_eq!(best([dnd]), Some(dnd));
        // Same show: a device in use beats one that has been idle, and a shorter idle beats a longer.
        let (long, short) = (detail(Show::Available, Some(90)), detail(Show::Available, Some(5)));
        assert_eq!(best([long, short, active]), Some(active));
        assert_eq!(best([long, short]), Some(short));
        assert_eq!(best([short, long]), Some(short), "order does not matter");
    }

    #[test]
    fn the_circle_is_green_yellow_red_or_grey() {
        let d = |s| Some(detail(s, None));
        assert_eq!(light(true, true, d(Show::Available)), Light::Green);
        assert_eq!(light(true, true, d(Show::Away)), Light::Yellow);
        assert_eq!(light(true, true, d(Show::DoNotDisturb)), Light::Red);
        assert_eq!(light(true, true, None), Light::Green, "online before any detail arrived");
        // Idle alone does not turn it yellow: that is what `show` is for.
        assert_eq!(light(true, true, Some(detail(Show::Available, Some(120)))), Light::Green);
        // Unknown or refused: presence is not shared with us. Offline is grey too.
        assert_eq!(light(false, true, d(Show::Available)), Light::Grey, "no subscription, whatever stale data says");
        assert_eq!(light(false, false, None), Light::Grey);
        assert_eq!(light(true, false, d(Show::DoNotDisturb)), Light::Grey, "offline");
        assert_eq!(light(true, false, None), Light::Grey);
    }

    #[test]
    fn every_colour_has_a_spoken_meaning_so_colour_is_never_the_only_signal() {
        let meanings = [Light::Green, Light::Yellow, Light::Red, Light::Grey].map(Light::meaning);
        assert_eq!(meanings, ["Online", "Away", "Do not disturb", "Status unknown"]);
        let unique: std::collections::HashSet<_> = meanings.iter().collect();
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn the_description_adds_how_long_they_have_been_idle() {
        let n = now();
        assert_eq!(describe(Light::Green, None, n), "Online");
        assert_eq!(describe(Light::Grey, Some(detail(Show::Away, Some(30))), n), "Status unknown", "grey says nothing else");
        // Idle for less than a minute is not worth saying.
        let fresh = Detail { show: Show::Available, idle_since: Some(n - Duration::try_seconds(30).unwrap()) };
        assert_eq!(describe(Light::Green, Some(fresh), n), "Online");
        let text = describe(Light::Yellow, Some(detail(Show::Away, Some(12))), n);
        assert!(text.starts_with("Away · idle for 12 minutes · since "), "{text}");
        let since = n - Duration::try_minutes(12).unwrap();
        assert!(text.ends_with(&crate::last_activity::absolute(&since.with_timezone(&chrono::Local))), "{text}");
        let text = describe(Light::Red, Some(detail(Show::DoNotDisturb, Some(60 * 5))), n);
        assert!(text.starts_with("Do not disturb · idle for 5 hours · since "), "{text}");
        let text = describe(Light::Green, Some(detail(Show::Available, Some(1))), n);
        assert!(text.starts_with("Online · idle for 1 minute · since "), "{text}");
        assert_eq!(describe(Light::Green, Some(detail(Show::Available, None)), n), "Online");
    }
}
