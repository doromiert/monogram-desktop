//! Historical mobile body convention only; never used when explicit poll XML is present.
//! Ported from mobile MonogramPolls.java at 9cde459ac, GPL-3.0-or-later.
use crate::polls::{OptionValue, Poll};
pub const EMOJI: [&str; 32] = [
    "1️⃣", "2️⃣", "3️⃣", "4️⃣", "5️⃣", "6️⃣", "7️⃣", "8️⃣", "9️⃣", "🔟", "🔴", "🟠", "🟡", "🟢", "🔵", "🟣",
    "🟤", "⚫", "⚪", "⭐", "🌙", "☀️", "🌈", "⚡", "🔥", "💧", "🌸", "🍀", "🍎", "🍋", "🍇", "🥝",
];
const SINGLE: &str = "Public votes. React with an option's emoji to vote. Choose one.";
const MULTIPLE: &str = "Public votes. React with option emojis to vote. Choose any.";
const CLOSED: &str = "Voting closed.";
fn field(text: &str, max: usize) -> bool {
    text.encode_utf16().count() <= max
        && !text.trim_matches(|c| c <= '\u{20}').is_empty()
        && !text.chars().any(|c| c.is_control() && c != '\n')
}
fn unindent(text: &str) -> Option<String> {
    let mut lines = text.split('\n');
    let mut value = lines.next()?.to_string();
    for line in lines {
        value.push('\n');
        value.push_str(line.strip_prefix("  ")?);
    }
    Some(value)
}
pub fn body(poll: &Poll) -> Option<String> {
    if !field(&poll.question, 4096)
        || !poll.description.is_empty() && !field(&poll.description, 4096)
        || !(2..=32).contains(&poll.options.len())
    {
        return None;
    }
    let indent = |s: &str| s.replace('\n', "\n  ");
    let mut text = format!("📊 {}\n\n", indent(&poll.question));
    if !poll.description.is_empty() {
        text.push_str(&format!("📝 {}\n\n", indent(&poll.description)));
    }
    for (index, option) in poll.options.iter().enumerate() {
        if !field(&option.label, 1024)
            || option.emoji.replace('\u{fe0f}', "") != EMOJI[index].replace('\u{fe0f}', "")
        {
            return None;
        }
        if index > 0 {
            text.push('\n');
        }
        text.push_str(&format!("{} {}", EMOJI[index], indent(&option.label)));
    }
    text.push_str("\n\n");
    text.push_str(if poll.multiple { MULTIPLE } else { SINGLE });
    if poll.closed {
        text.push('\n');
        text.push_str(CLOSED);
    }
    (text.encode_utf16().count() <= 40000).then_some(text)
}
pub fn parse(text: &str) -> Option<Poll> {
    if text.encode_utf16().count() > 40000 || !text.starts_with("📊 ") {
        return None;
    }
    let first = text.find("\n\n")?;
    let last = text.rfind("\n\n")?;
    if last <= first {
        return None;
    }
    let footer = &text[last + 2..];
    let closed = footer.ends_with(&format!("\n{CLOSED}"));
    let footer = if closed {
        footer.strip_suffix(&format!("\n{CLOSED}"))?
    } else {
        footer
    };
    if footer != SINGLE && footer != MULTIPLE {
        return None;
    }
    let question = unindent(&text["📊 ".len()..first])?;
    let mut start = first + 2;
    let mut description = String::new();
    if text[start..].starts_with("📝 ") {
        let end = start + text[start..].find("\n\n")?;
        if end >= last {
            return None;
        }
        description = unindent(&text[start + "📝 ".len()..end])?;
        start = end + 2;
    }
    let mut labels = Vec::new();
    let mut answer: Option<String> = None;
    for line in text[start..last].split('\n') {
        if line.starts_with("  ")
            && let Some(value) = answer.as_mut()
        {
            value.push('\n');
            value.push_str(line);
            continue;
        }
        if let Some(value) = answer.take() {
            labels.push(unindent(&value)?);
        }
        let emoji = EMOJI.get(labels.len())?;
        answer = Some(line.strip_prefix(&format!("{emoji} "))?.to_string());
    }
    if let Some(value) = answer {
        labels.push(unindent(&value)?);
    }
    let poll = Poll {
        question,
        description,
        options: labels
            .into_iter()
            .enumerate()
            .map(|(i, label)| OptionValue {
                emoji: EMOJI[i].into(),
                label,
            })
            .collect(),
        multiple: footer == MULTIPLE,
        closed,
    };
    (body(&poll).as_deref() == Some(text)).then_some(poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::Message, polls::Source};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use tokio_xmpp::minidom::Element;
    fn vectors() -> Vec<serde_json::Value> {
        serde_json::from_str(include_str!(
            "../tests/mobile-reference/legacy-poll-vectors.json"
        ))
        .unwrap()
    }
    fn decoded(value: &serde_json::Value, key: &str) -> String {
        String::from_utf8(STANDARD.decode(value[key].as_str().unwrap()).unwrap()).unwrap()
    }
    fn stanza(body: &str) -> Element {
        Element::builder("message", "jabber:client")
            .attr("from".try_into().unwrap(), "alice@example.org/old-mobile")
            .attr("to".try_into().unwrap(), "bob@example.org")
            .attr("type".try_into().unwrap(), "chat")
            .attr("id".try_into().unwrap(), "historical")
            .append(
                Element::builder("body", "jabber:client")
                    .append(body)
                    .build(),
            )
            .build()
    }
    #[test]
    fn actual_mobile_legacy_codec_values_and_utf16_bounds() {
        for value in vectors() {
            let raw = decoded(&value, "body");
            let parsed = parse(&raw);
            assert_eq!(parsed.is_some(), value["valid"].as_bool().unwrap());
            let (_, message, _) =
                crate::protocol::normalize(&stanza(&raw), "bob@example.org", None).unwrap();
            assert_eq!(message.text, raw);
            if let Some(mut poll) = parsed {
                assert_eq!(poll.question, decoded(&value, "question"));
                assert_eq!(poll.description, decoded(&value, "description"));
                assert_eq!(poll.multiple, value["multiple"].as_bool().unwrap());
                assert_eq!(poll.closed, value["closed"].as_bool().unwrap());
                let expected: Vec<_> = value["options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| OptionValue {
                        emoji: String::from_utf8(STANDARD.decode(p[0].as_str().unwrap()).unwrap())
                            .unwrap(),
                        label: String::from_utf8(STANDARD.decode(p[1].as_str().unwrap()).unwrap())
                            .unwrap(),
                    })
                    .collect();
                assert_eq!(poll.options, expected);
                assert_eq!(message.poll_source(), Source::Legacy);
                assert_eq!(message.poll, Some(poll.clone()));
                assert_eq!(message.content_text(), raw);
                assert_eq!(body(&poll).as_deref(), Some(raw.as_str()));
                poll.closed = true;
                assert_eq!(body(&poll).is_some(), value["can_close"].as_bool().unwrap());
                if value["can_close"] == true {
                    assert_eq!(body(&poll).unwrap(), decoded(&value, "closed_body"));
                }
            } else {
                assert!(message.poll.is_none());
                assert_eq!(message.poll_source(), Source::Text);
            }
        }
    }
    #[test]
    fn explicit_metadata_and_attachments_never_fall_back_to_body_guessing() {
        let value = vectors().remove(0);
        let raw = decoded(&value, "body");
        for extension in [
            Element::builder("poll", crate::polls::NS)
                .attr("type".try_into().unwrap(), "quiz")
                .build(),
            Element::builder("file-sharing", "urn:xmpp:sfs:0").build(),
        ] {
            let mut xml = stanza(&raw);
            xml.append_child(extension);
            let (_, message, _) =
                crate::protocol::normalize(&xml, "bob@example.org", None).unwrap();
            assert!(message.poll.is_none());
            assert_eq!(message.text, raw);
            assert!(
                Message::decode(&serde_json::to_string(&message).unwrap())
                    .unwrap()
                    .poll
                    .is_none()
            );
        }
        let mut xml = stanza(&raw);
        let mut explicit = parse(&raw).unwrap();
        explicit.question = "The explicit question".into();
        xml.append_child(explicit.element());
        let (_, message, _) = crate::protocol::normalize(&xml, "bob@example.org", None).unwrap();
        assert_eq!(message.poll_source(), Source::Structured);
        assert_eq!(message.poll.unwrap().question, "The explicit question");
        assert_eq!(message.text, raw);
    }
    #[test]
    fn old_cached_messages_and_reply_quotes_preserve_legacy_identity() {
        let raw = decoded(&vectors()[0], "body");
        let xml = crate::protocol::text_stanza(
            "bob@example.org",
            &raw,
            "old",
            Some(("quoted", "bob@example.org", "Quote 🦊")),
        );
        let mut xml = xml;
        xml.set_attr(
            "".into(),
            "from".try_into().unwrap(),
            "alice@example.org/old-mobile",
        );
        let (_, message, _) = crate::protocol::normalize(&xml, "bob@example.org", None).unwrap();
        assert_eq!(message.poll_source(), Source::Legacy);
        assert_eq!(message.text, raw);
        assert_eq!(message.reply_id, "quoted");
        let mut json = serde_json::to_value(&message).unwrap();
        let object = json.as_object_mut().unwrap();
        for key in ["poll", "poll_source", "poll_fallback"] {
            object.remove(key);
        }
        object.insert("kind".into(), "text".into());
        let restored = Message::decode(&json.to_string()).unwrap();
        assert_eq!(restored.poll_source(), Source::Legacy);
        assert_eq!(restored.poll, message.poll);
        assert_eq!(restored.text, raw);
        let outgoing = crate::protocol::outgoing("bob@example.org", &message, "Quote 🦊");
        assert!(outgoing.get_child("poll", crate::polls::NS).is_none());
        assert!(
            outgoing
                .get_child("body", "jabber:client")
                .unwrap()
                .text()
                .ends_with(&raw)
        );
    }
}
