//! XEP-0012 Last Activity: when a contact last went offline, as "Last seen 15 minutes ago ·
//! Sat July 4 2015 23:25".
//!
//! Asking a bare address is answered by the contact's server: `seconds` is how long ago they
//! last disconnected, or `0` while at least one of their resources is online. A server only
//! answers people allowed to see the contact's presence; everyone else gets `forbidden`, and
//! so does not learn anything here either. Whether the server speaks the protocol at all is
//! advertised as the `jabber:iq:last` disco feature.
use chrono::{DateTime, Duration, Local, TimeZone, Utc};
use tokio_xmpp::minidom::Element;

pub const NS: &str = "jabber:iq:last";
/// A server claiming someone was last seen more than this long ago is ignored, not believed.
const MAX_SECONDS: u64 = 100 * 365 * 24 * 3600;

/// The question to put to a contact's bare address.
pub fn request() -> Element {
    Element::builder("query", NS).build()
}

/// What the answer said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    /// At least one of their resources is connected right now.
    Online,
    /// They were last connected this many seconds before the answer.
    Away { seconds: u64 },
}

/// The `<query/>` of a result stanza, or `None` for anything that is not a plain number of
/// seconds (a negative, fractional, huge, non-numeric or missing value, or the wrong namespace).
pub fn parse(query: &Element) -> Option<Activity> {
    if !query.is("query", NS) {
        return None;
    }
    let text = query.attr("seconds")?.trim();
    if text.is_empty() || text.len() > 12 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match text.parse::<u64>().ok()? {
        0 => Some(Activity::Online),
        seconds if seconds <= MAX_SECONDS => Some(Activity::Away { seconds }),
        _ => None,
    }
}

/// Whether a disco#info `query` lists the protocol.
pub fn advertised(info: &Element) -> bool {
    info.children().any(|child| {
        child.is("feature", "http://jabber.org/protocol/disco#info") && child.attr("var") == Some(NS)
    })
}

/// "Sat July 4 2015 23:25": weekday, month in full, day without a leading zero, year, 24-hour time.
pub fn absolute<Tz: TimeZone>(then: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    then.format("%a %B %-d %Y %H:%M").to_string()
}

fn plural(n: i64, unit: &str) -> String {
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// How long `then` was, in the largest whole unit and without "ago": "less than a minute",
/// "15 minutes", "2 hours", "3 days", "5 months", "2 years". A moment in the future (a clock that
/// is slightly off) reads as no time at all.
pub fn elapsed<Tz: TimeZone, Tz2: TimeZone>(then: &DateTime<Tz>, now: &DateTime<Tz2>) -> String {
    let seconds = now.clone().signed_duration_since(then.clone()).num_seconds().max(0);
    match seconds {
        0..=59 => "less than a minute".into(),
        60..=3599 => plural(seconds / 60, "minute"),
        3600..=86_399 => plural(seconds / 3600, "hour"),
        86_400..=2_591_999 => plural(seconds / 86_400, "day"),
        2_592_000..=31_535_999 => plural(seconds / 2_592_000, "month"),
        _ => plural(seconds / 31_536_000, "year"),
    }
}

/// How long ago `then` was: "just now", "15 minutes ago", "2 hours ago", "3 days ago", ...
pub fn relative<Tz: TimeZone, Tz2: TimeZone>(then: &DateTime<Tz>, now: &DateTime<Tz2>) -> String {
    match elapsed(then, now).as_str() {
        "less than a minute" => "just now".into(),
        text => format!("{text} ago"),
    }
}

/// The moment of last activity for an answer received at `received`.
pub fn when(activity: Activity, received: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match activity {
        Activity::Online => None,
        Activity::Away { seconds } => {
            received.checked_sub_signed(Duration::try_seconds(i64::try_from(seconds).ok()?)?)
        }
    }
}

/// The line shown to people, from the last known moment (`None` means online now).
pub fn line(last_seen: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    match last_seen {
        None => "Online".into(),
        Some(then) => {
            let local = then.with_timezone(&Local);
            format!("Last seen {} · {}", relative(&then, &now), absolute(&local))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate};

    fn query(seconds: &str) -> Element {
        format!("<query xmlns='jabber:iq:last' seconds='{seconds}'>Heading Home</query>").parse().unwrap()
    }
    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(&NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap())
    }

    #[test]
    fn the_spec_example_is_fifteen_minutes_and_three_seconds() {
        assert_eq!(parse(&query("903")), Some(Activity::Away { seconds: 903 }));
        assert_eq!(request().ns(), NS);
        assert_eq!(request().name(), "query");
    }

    #[test]
    fn zero_means_online_and_odd_values_mean_nothing() {
        assert_eq!(parse(&query("0")), Some(Activity::Online));
        assert_eq!(parse(&query("00")), Some(Activity::Online));
        assert_eq!(parse(&query(" 42 ")), Some(Activity::Away { seconds: 42 }));
        for bad in ["", " ", "-1", "+5", "1.5", "1e3", "abc", "NaN", "0x10", "١٢", "9".repeat(13).as_str(), "99999999999"] {
            assert_eq!(parse(&query(bad)), None, "{bad:?}");
        }
        assert_eq!(parse(&format!("<query xmlns='jabber:iq:last'/>").parse().unwrap()), None, "no seconds");
        assert_eq!(parse(&"<query xmlns='urn:other' seconds='5'/>".parse().unwrap()), None, "wrong namespace");
        assert_eq!(parse(&"<other xmlns='jabber:iq:last' seconds='5'/>".parse().unwrap()), None, "wrong element");
        assert_eq!(parse(&query(&MAX_SECONDS.to_string())), Some(Activity::Away { seconds: MAX_SECONDS }));
        assert_eq!(parse(&query(&(MAX_SECONDS + 1).to_string())), None, "over a hundred years");
    }

    #[test]
    fn the_absolute_format_matches_the_example() {
        // Sat July 4 2015 23:25: no zero padding on the day, 24-hour time, padded minutes.
        assert_eq!(absolute(&at(2015, 7, 4, 23, 25)), "Sat July 4 2015 23:25");
        assert_eq!(absolute(&at(2024, 1, 1, 0, 5)), "Mon January 1 2024 00:05");
        assert_eq!(absolute(&at(2024, 12, 31, 9, 0)), "Tue December 31 2024 09:00");
        assert_eq!(absolute(&at(2000, 2, 29, 12, 59)), "Tue February 29 2000 12:59");
        // It prints the time in the zone it is given.
        let plus_two = FixedOffset::east_opt(2 * 3600).unwrap();
        assert_eq!(absolute(&at(2015, 7, 4, 23, 25).with_timezone(&plus_two)), "Sun July 5 2015 01:25");
    }

    #[test]
    fn relative_time_uses_the_largest_whole_unit_and_the_right_plural() {
        let now = at(2025, 6, 15, 12, 0);
        let ago = |seconds: i64| relative(&(now - Duration::try_seconds(seconds).unwrap()), &now);
        for (seconds, want) in [
            (0, "just now"),
            (1, "just now"),
            (59, "just now"),
            (60, "1 minute ago"),
            (119, "1 minute ago"),
            (120, "2 minutes ago"),
            (903, "15 minutes ago"),
            (3599, "59 minutes ago"),
            (3600, "1 hour ago"),
            (7199, "1 hour ago"),
            (7200, "2 hours ago"),
            (86_399, "23 hours ago"),
            (86_400, "1 day ago"),
            (172_800, "2 days ago"),
            (2_591_999, "29 days ago"),
            (2_592_000, "1 month ago"),
            (5_184_000, "2 months ago"),
            (31_535_999, "12 months ago"),
            (31_536_000, "1 year ago"),
            (63_072_000, "2 years ago"),
            (3_153_600_000, "100 years ago"),
        ] {
            assert_eq!(ago(seconds), want, "{seconds}s");
        }
        assert_eq!(ago(-500), "just now", "a clock that runs ahead never says 'in the future'");
    }

    #[test]
    fn elapsed_time_has_no_ago_and_matches_relative() {
        let now = at(2025, 6, 15, 12, 0);
        let gap = |seconds: i64| elapsed(&(now - Duration::try_seconds(seconds).unwrap()), &now);
        assert_eq!(gap(0), "less than a minute");
        assert_eq!(gap(59), "less than a minute");
        assert_eq!(gap(60), "1 minute");
        assert_eq!(gap(720), "12 minutes");
        assert_eq!(gap(3600), "1 hour");
        assert_eq!(gap(86_400 * 3), "3 days");
        assert_eq!(gap(-90), "less than a minute", "a clock that runs ahead");
        for seconds in [0, 59, 60, 61, 3599, 3600, 86_400, 2_592_000, 31_536_000] {
            let then = now - Duration::try_seconds(seconds).unwrap();
            let want = if seconds < 60 { "just now".to_string() } else { format!("{} ago", elapsed(&then, &now)) };
            assert_eq!(relative(&then, &now), want, "{seconds}");
        }
    }

    #[test]
    fn the_moment_of_last_activity_is_the_answer_time_minus_the_seconds() {
        let received = at(2015, 7, 4, 23, 40);
        assert_eq!(when(Activity::Away { seconds: 903 }, received), Some(received - Duration::try_seconds(903).unwrap()));
        assert_eq!(when(Activity::Online, received), None);
        assert_eq!(when(Activity::Away { seconds: 0 }, received), Some(received));
        // The widest value the parser lets through still produces a valid date.
        assert!(when(Activity::Away { seconds: MAX_SECONDS }, received).is_some());
        assert_eq!(when(Activity::Away { seconds: u64::MAX }, received), None, "no overflow");
    }

    #[test]
    fn the_line_shows_relative_and_absolute_together_or_just_online() {
        let now = at(2015, 7, 5, 0, 0);
        assert_eq!(line(None, now), "Online");
        let then = at(2015, 7, 4, 23, 25);
        let text = line(Some(then), now);
        assert!(text.starts_with("Last seen 35 minutes ago · "), "{text}");
        let local = then.with_timezone(&Local);
        assert!(text.ends_with(&absolute(&local)), "{text}");
        assert_eq!(text.matches(" · ").count(), 1);
        // Months later it still reads sensibly.
        let text = line(Some(at(2015, 1, 9, 8, 3)), at(2015, 7, 5, 0, 0));
        assert!(text.starts_with("Last seen 5 months ago · "), "{text}");
    }

    #[test]
    fn support_is_read_from_the_disco_features() {
        let info = |inner: &str| -> Element {
            format!("<query xmlns='http://jabber.org/protocol/disco#info'>{inner}</query>").parse().unwrap()
        };
        assert!(advertised(&info("<feature var='jabber:iq:last'/>")));
        assert!(advertised(&info("<identity category='server' type='im'/><feature var='x'/><feature var='jabber:iq:last'/>")));
        assert!(!advertised(&info("")));
        assert!(!advertised(&info("<feature var='jabber:iq:lastx'/>")));
        assert!(!advertised(&info("<identity var='jabber:iq:last'/>")));
    }
}
