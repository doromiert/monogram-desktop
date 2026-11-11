//! Display-only pairing of our explicitly linked Legacy OMEMO upload captions.
use crate::{encryption_policy::Mode, model::Message};
use std::collections::{HashMap, HashSet};

/// The caller supplies one account/conversation's loaded window. Missing parents
/// leave their companions visible; neither wire message is changed or removed.
pub(crate) fn display_messages(messages: &[Message]) -> Vec<Message> {
    let completed = |message: &Message| {
        message.own
            && message.mode() == Mode::Legacy
            && !message.retracted
            && !message.unavailable
            && message.ciphertext.is_empty()
            && message.upload.is_none()
            && message.action.is_none()
            && matches!(message.delivery.as_str(), "sent" | "delivered" | "read")
    };
    let mut parents = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        if completed(message)
            && !message.wire_id.is_empty()
            && message
                .attachment
                .as_ref()
                .is_some_and(|file| file.link().is_some_and(|link| link.encrypted()))
        {
            parents
                .entry(message.wire_id.as_str())
                .and_modify(|parent| *parent = None)
                .or_insert(Some(index));
        }
    }
    let mut captions = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        if completed(message)
            && message.kind == "text"
            && message.attachment.is_none()
            && message.poll.is_none()
            && !message.upload_caption_for.is_empty()
            && let Some(Some(parent)) = parents.get(message.upload_caption_for.as_str())
            && *parent != index
        {
            // Ambiguous local metadata must never hide a message.
            captions
                .entry(*parent)
                .and_modify(|caption| *caption = None)
                .or_insert(Some(index));
        }
    }
    let mut display = messages.to_vec();
    let mut hidden = HashSet::new();
    for (parent, caption) in captions {
        if let Some(caption) = caption {
            display[parent].text = messages[caption].text.clone();
            hidden.insert(caption);
        }
    }
    display
        .into_iter()
        .enumerate()
        .filter_map(|(index, message)| (!hidden.contains(&index)).then_some(message))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> Vec<Message> {
        let mut parent = Message {
            id: "media".into(),
            wire_id: "media".into(),
            own: true,
            encrypted: true,
            encryption_mode: Some(Mode::Legacy),
            delivery: "sent".into(),
            kind: "file".into(),
            attachment: crate::file_share::Attachment::bare(&format!(
                "aesgcm://example.invalid/image.png#{}",
                "12".repeat(44)
            )),
            ..Default::default()
        };
        parent.attachment.as_mut().unwrap().mime = "image/png".into();
        let caption = Message {
            id: "caption".into(),
            wire_id: "caption".into(),
            own: true,
            encrypted: true,
            encryption_mode: Some(Mode::Legacy),
            delivery: "sent".into(),
            upload_caption_for: "media".into(),
            text: "Caption".into(),
            ..Default::default()
        };
        vec![parent, caption]
    }

    #[test]
    fn explicit_pair_survives_reload_and_tracks_edit_retraction_and_window() {
        let original = pair();
        let mut messages: Vec<Message> =
            serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        let merged = display_messages(&messages);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "media");
        assert_eq!(merged[0].text, "Caption");
        assert!(messages[0].text.is_empty());
        messages[1].text = "Edited caption".into();
        assert_eq!(display_messages(&messages)[0].text, "Edited caption");
        messages[1].retracted = true;
        assert!(display_messages(&messages)[0].text.is_empty());
        messages[1].retracted = false;
        messages[0].retracted = true;
        assert_eq!(display_messages(&messages).len(), 2);
        assert_eq!(display_messages(&messages[1..])[0].text, "Edited caption");
    }

    #[test]
    fn no_incoming_temporal_pending_wrong_protocol_or_ambiguous_guessing() {
        let original = pair();
        for invalid in 0..7 {
            let mut messages = original.clone();
            match invalid {
                0 => messages[1].upload_caption_for.clear(),
                1 => messages[1].own = false,
                2 => messages[0].encryption_mode = Some(Mode::Omemo2),
                3 => messages[1].delivery = "failed".into(),
                4 => messages[1].unavailable = true,
                5 => {
                    messages[0].attachment.as_mut().unwrap().wire_url =
                        "https://example.invalid/image.png".into()
                }
                _ => messages.push(messages[1].clone()),
            }
            assert_eq!(
                display_messages(&messages).len(),
                messages.len(),
                "invalid case {invalid}"
            );
        }
    }
}
