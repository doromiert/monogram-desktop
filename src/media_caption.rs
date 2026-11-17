//! Presentation links for Legacy OMEMO captions and mobile forwarded-file markers.
use crate::{encryption_policy::Mode, model::Message};
use rusqlite::params;
use std::collections::{HashMap, HashSet};

fn readable(message: &Message) -> bool {
    message.encrypted
        && message.mode() == Mode::Legacy
        && !message.retracted
        && !message.unavailable
        && message.ciphertext.is_empty()
        && message.upload.is_none()
        && message.action.is_none()
        && (!message.own || matches!(message.delivery.as_str(), "sent" | "delivered" | "read"))
        && !matches!(
            message.delivery.as_str(),
            "failed" | "pending" | "waiting-upload" | "uncertain"
        )
}
fn forward_marker(message: &Message) -> bool {
    readable(message)
        && message.kind == "text"
        && message.text == "Forwarded attachment"
        && message.forwarded.is_some()
        && message.attachment.is_none()
        && message.poll.is_none()
        && message.reply.is_empty()
        && message.reply_id.is_empty()
        && message.reply_to.is_empty()
}
fn mobile_pair(parent: &Message, marker: &Message) -> bool {
    if !forward_marker(marker)
        || !readable(parent)
        || parent.own != marker.own
        || parent.sender.is_empty()
        || parent.sender != marker.sender
        || parent.fingerprint.is_empty()
        || parent.fingerprint != marker.fingerprint
        || parent.wire_id.is_empty()
        || parent.id == marker.id
        || !parent.text.is_empty() && marker.upload_caption_for.is_empty()
        || parent
            .forwarded
            .as_ref()
            .is_some_and(|a| Some(a) != marker.forwarded.as_ref())
        || !marker.upload_caption_for.is_empty() && marker.upload_caption_for != parent.wire_id
        || !parent
            .attachment
            .as_ref()
            .is_some_and(|a| a.link().is_some_and(|l| l.encrypted()))
    {
        return false;
    }
    match (
        chrono::DateTime::parse_from_rfc3339(&parent.timestamp),
        chrono::DateTime::parse_from_rfc3339(&marker.timestamp),
    ) {
        (Ok(parent), Ok(marker)) => (0..=30_000).contains(&(marker - parent).num_milliseconds()),
        _ => false,
    }
}
fn recover_loaded(messages: &mut [Message]) {
    for index in 1..messages.len() {
        let (parents, captions) = messages.split_at_mut(index);
        let (parent, caption) = (&mut parents[index - 1], &mut captions[0]);
        if mobile_pair(parent, caption) {
            parent.forwarded = caption.forwarded.clone();
            caption.upload_caption_for = parent.wire_id.clone();
        }
    }
}
impl crate::storage::Store {
    pub(crate) fn forwarded_attachment_neighbor(
        &self,
        account: &str,
        peer: &str,
        message: &Message,
    ) -> rusqlite::Result<Option<Message>> {
        if !forward_marker(message) && !(readable(message) && message.attachment.is_some()) {
            return Ok(None);
        }
        let (comparison, direction) = if forward_marker(message) {
            ("<", "DESC")
        } else {
            (">", "ASC")
        };
        let sql = format!(
            "SELECT body FROM messages WHERE account=? AND peer=? AND (timestamp,id){comparison}(?,?) ORDER BY timestamp {direction},id {direction} LIMIT 1"
        );
        use rusqlite::OptionalExtension;
        let neighbor: Option<String> = self
            .db
            .query_row(
                &sql,
                params![account, peer, message.timestamp, message.id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(neighbor) = neighbor.and_then(|s| Message::decode(&s).ok()) else {
            return Ok(None);
        };
        let paired = if forward_marker(message) {
            mobile_pair(&neighbor, message)
        } else {
            mobile_pair(message, &neighbor)
        };
        Ok(paired.then_some(neighbor))
    }
    /// Derive presentation metadata only; preserve both authenticated wire bodies and IDs.
    pub(crate) fn recover_forwarded_attachment(
        &self,
        account: &str,
        peer: &str,
        message: &Message,
    ) -> rusqlite::Result<()> {
        let Some(neighbor) = self.forwarded_attachment_neighbor(account, peer, message)? else {
            return Ok(());
        };
        let (parent, marker) = if forward_marker(message) {
            (&neighbor, message)
        } else {
            (message, &neighbor)
        };
        if parent.forwarded == marker.forwarded && marker.upload_caption_for == parent.wire_id {
            return Ok(());
        }
        let attribution = serde_json::to_string(&marker.forwarded).unwrap();
        for table in ["messages", "message_bases"] {
            self.db.execute(&format!("UPDATE {table} SET body=CASE WHEN id=? THEN json_set(body,'$.forwarded',json(?)) ELSE json_set(body,'$.upload_caption_for',?) END WHERE account=? AND peer=? AND id IN (?,?)"),
                params![parent.id,attribution,parent.wire_id,account,peer,parent.id,marker.id])?;
        }
        Ok(())
    }
}

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
    let mut display = messages.to_vec();
    recover_loaded(&mut display);
    let mut parents = HashMap::new();
    for (index, message) in display.iter().enumerate() {
        if readable(message)
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
    for (index, message) in display.iter().enumerate() {
        if (completed(message) || forward_marker(message))
            && message.kind == "text"
            && message.attachment.is_none()
            && message.poll.is_none()
            && !message.upload_caption_for.is_empty()
            && let Some(Some(parent)) = parents.get(message.upload_caption_for.as_str())
            && *parent != index
            && display[*parent].own == message.own
        {
            // Ambiguous local metadata must never hide a message.
            captions
                .entry(*parent)
                .and_modify(|caption| *caption = None)
                .or_insert(Some(index));
        }
    }
    let mut hidden = HashSet::new();
    for (parent, caption) in captions {
        if let Some(caption) = caption {
            let marker = forward_marker(&messages[caption]);
            if !marker {
                display[parent].text = messages[caption].text.clone();
                display[parent].formatting = messages[caption].formatting.clone();
                display[parent].preview_spoilers = messages[caption].preview_spoilers.clone();
            }
            if let Some(attribution) = &messages[caption].forwarded {
                display[parent].forwarded = Some(attribution.clone());
            }
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

    fn mobile_forward(own: bool) -> Vec<Message> {
        let mut messages = pair();
        for (index, message) in messages.iter_mut().enumerate() {
            message.own = own;
            if !own {
                message.delivery.clear();
            }
            message.sender = if own {
                "owner@fixture.invalid/phone"
            } else {
                "peer@fixture.invalid/phone"
            }
            .into();
            message.fingerprint = "authenticated-phone-key".into();
            message.timestamp = format!("2026-11-17T17:06:0{index}Z");
        }
        messages[1].upload_caption_for.clear();
        messages[1].text = "Forwarded attachment".into();
        messages[1].forwarded = crate::forwarding::Attribution::new(
            "room@conference.fixture.invalid/Original",
            1700000000,
        );
        messages
    }
    #[test]
    fn mobile_marker_becomes_attachment_attribution_without_changing_wire_bodies() {
        for own in [false, true] {
            let original = mobile_forward(own);
            let display = display_messages(&original);
            assert_eq!(display.len(), 1);
            assert_eq!(display[0].id, "media");
            assert!(display[0].text.is_empty());
            assert_eq!(display[0].forwarded, original[1].forwarded);
            assert_eq!(original[1].text, "Forwarded attachment");
            assert!(original[1].upload_caption_for.is_empty());
            assert_eq!(
                display_messages(&original[1..]).len(),
                1,
                "do not hide a marker whose attachment is outside the window"
            );
        }
        let mut edited = mobile_forward(true);
        recover_loaded(&mut edited);
        edited[0].text = "Edited media caption".into();
        assert_eq!(display_messages(&edited)[0].text, "Edited media caption");
        for invalid in 0..14 {
            let mut messages = mobile_forward(true);
            match invalid {
                0 => messages[1].own = false,
                1 => messages[1].sender = "another@fixture.invalid/phone".into(),
                2 => messages[1].fingerprint = "different-device".into(),
                3 => messages[0].fingerprint.clear(),
                4 => messages[1].encryption_mode = Some(Mode::Omemo2),
                5 => messages[1].timestamp = "2026-11-17T17:06:31Z".into(),
                6 => messages[1].timestamp = "2026-11-17T17:05:59Z".into(),
                7 => messages[1].text = "An actual forwarded message".into(),
                8 => messages[1].forwarded = None,
                9 => messages[0].text = "Keep my original caption".into(),
                10 => messages[1].retracted = true,
                11 => messages[1].delivery = "failed".into(),
                12 => {
                    messages[0].attachment.as_mut().unwrap().wire_url =
                        "https://fixture.invalid/plain.png".into()
                }
                _ => messages[1].upload_caption_for = "different-parent".into(),
            }
            assert_eq!(
                display_messages(&messages).len(),
                2,
                "unsafe pair {invalid}"
            );
        }
    }
    #[test]
    fn mobile_links_recover_live_out_of_order_and_old_history_and_expand_deletion() {
        use crate::{
            model::{Account, State},
            storage::Store,
        };
        const OWNER: &str = "owner@fixture.invalid";
        const PEER: &str = "peer@fixture.invalid";
        for (own, reverse) in [(false, false), (false, true), (true, false), (true, true)] {
            let store = Store::open(true).unwrap();
            store
                .save_account(&Account {
                    jid: OWNER.into(),
                    host: String::new(),
                    port: 5222,
                    direct_tls: false,
                })
                .unwrap();
            let original = mobile_forward(own);
            let mut state = State::live(OWNER, vec![]);
            let chat = state.chat_for(PEER);
            let order = if reverse { [1, 0] } else { [0, 1] };
            for index in order {
                store
                    .save_chat_message(OWNER, chat, Some(&original[index]))
                    .unwrap();
            }
            let parent = store.message(OWNER, PEER, "media").unwrap();
            let marker = store.message(OWNER, PEER, "caption").unwrap();
            assert_eq!(parent.forwarded, original[1].forwarded);
            assert_eq!(marker.upload_caption_for, "media");
            assert_eq!(marker.text, "Forwarded attachment");
            assert_eq!(
                store
                    .deletion_targets(OWNER, PEER, &[parent.id])
                    .unwrap()
                    .len(),
                2
            );
            // Simulate history saved before this compatibility handler existed.
            for table in ["messages", "message_bases"] {
                store.db.execute(&format!("UPDATE {table} SET body=json_remove(body,'$.upload_caption_for') WHERE account=? AND peer=?"),rusqlite::params![OWNER,PEER]).unwrap();
                store.db.execute(&format!("UPDATE {table} SET body=json_remove(body,'$.forwarded') WHERE account=? AND peer=? AND id='media'"),rusqlite::params![OWNER,PEER]).unwrap();
            }
            store.db.pragma_update(None, "query_only", true).unwrap();
            let history = store.try_history(OWNER, PEER, None, 100).unwrap();
            assert_eq!(display_messages(&history).len(), 1);
            assert!(
                store
                    .message(OWNER, PEER, "caption")
                    .unwrap()
                    .upload_caption_for
                    .is_empty(),
                "reading old history is presentation-only"
            );
            assert_eq!(
                store
                    .deletion_targets(OWNER, PEER, &["media".into()])
                    .unwrap()
                    .len(),
                2
            );
        }
    }
}
