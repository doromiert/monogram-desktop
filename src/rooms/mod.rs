//! Live room bookmarks and XEP-0045 membership. A bookmark alone never authorizes sending.
pub mod archive;
mod bookmarks;
pub mod discovery;
pub mod hats;
pub mod invitations;
pub mod manage;
pub mod members;
mod muc;

pub use bookmarks::{
    Bookmark, BookmarkInfo, bookmark_changed, load_bookmarks, save_bookmark, subscribe_bookmarks,
};
pub use manage::Password;
pub use muc::{Presence, Rooms};

#[derive(Clone, Debug)]
pub struct LocalBookmark {
    pub info: BookmarkInfo,
    pub revision: u64,
    pub dirty: bool,
}

use crate::iq;
use std::fmt;
use tokio_xmpp::{jid::Jid, minidom::Element};
const CLIENT: &str = "jabber:client";
const MAX_XML: usize = 1024 * 1024;

const NOT_PRIVATE: &str =
    "This room is not members-only and non-anonymous, so it cannot be end-to-end encrypted";
const OCCUPANT_NOT_LISTED: &str =
    " is in this room but is not a listed member or hides their address, so the message cannot be encrypted for them";
/// What the user can open to deal with a room failure shown as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    /// Conversation Security, to choose how this room is sent.
    Security,
    /// The participants panel, to see who is in the room.
    Participants,
}
/// The place that deals with a room failure, for the text the user saw.
pub fn fix_for(reason: &str) -> Option<Fix> {
    if reason == NOT_PRIVATE {
        Some(Fix::Security)
    } else if reason.ends_with(OCCUPANT_NOT_LISTED) {
        Some(Fix::Participants)
    } else {
        None
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Request(iq::Error),
    Joining,
    AlreadyJoined,
    NotJoined,
    Forbidden,
    Locked,
    Encryption,
    /// The room's member list is not ready, or changed while the message was prepared.
    RecipientsUnavailable,
    /// The room is not members-only and non-anonymous, so it can never be end-to-end encrypted.
    NotPrivate,
    /// Someone in the room is not a listed member or hides their address (the nickname).
    OccupantNotListed(String),
    BookmarkPrivacyUnavailable,
    Cancelled,
    OperationBusy,
    PartialCreated { room: String },
    JoinedSecretUnsaved { room: String },
    RoomSecretUnavailable,
    UnsupportedMessage,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::PartialCreated { room } = self {
            return write!(
                f,
                "The room {room} was created, but setup could not be completed. Its owner can configure it and rejoin."
            );
        }
        if let Self::JoinedSecretUnsaved { room } = self {
            return write!(
                f,
                "Joined {room}, but its password could not be saved in the system keyring. The room is still joined."
            );
        }
        if let Self::OccupantNotListed(nick) = self {
            return write!(f, "{nick}{OCCUPANT_NOT_LISTED}");
        }
        if let Self::Request(iq::Error::Rejected { condition, .. }) = self {
            return write!(
                f,
                "The server refused the room request: {}",
                condition.replace('-', " ")
            );
        }
        f.write_str(match self {
            Self::Invalid => "Invalid room address, nickname or server response",
            Self::Request(iq::Error::Timeout) => {
                "The server did not answer the room request"
            }
            Self::Request(iq::Error::Closed) => "Reconnect to synchronize room information",
            Self::Request(_) => "Could not synchronize room information",
            Self::Joining => "Waiting for the room to confirm membership",
            Self::AlreadyJoined => "You already joined this room",
            Self::NotJoined => "Join the room before sending messages",
            Self::Forbidden => "Your room role does not permit sending messages",
            Self::Locked => "The room requires configuration before you can send messages",
            Self::Encryption => {
                "Encrypted room sending is unavailable. Your encryption setting is unchanged."
            }
            Self::RecipientsUnavailable => "The room's member list is not ready or changed while the message was prepared; try again",
            Self::NotPrivate => NOT_PRIVATE,
            Self::OccupantNotListed(_) => unreachable!("written above"),
            Self::BookmarkPrivacyUnavailable => "Private bookmark synchronization is unavailable. This room choice stays on this device.",
            Self::Cancelled => "The room operation was cancelled because the connection or room changed.",
            Self::OperationBusy => "Another operation for this room is still running.",
            Self::RoomSecretUnavailable => "The system keyring could not read the saved room password. Unlock it or enter a password explicitly.",
            Self::PartialCreated {..} | Self::JoinedSecretUnsaved {..} => unreachable!(),
            Self::UnsupportedMessage => "This room message type is unavailable",
        })
    }
}
impl std::error::Error for Error {}
impl From<iq::Error> for Error {
    fn from(error: iq::Error) -> Self {
        Self::Request(error)
    }
}
type Result<T> = std::result::Result<T, Error>;

fn bare(value: &str) -> Result<String> {
    let jid = crate::protocol::bare(value).map_err(|_| Error::Invalid)?;
    if jid != value {
        return Err(Error::Invalid);
    }
    Ok(jid)
}
fn addressed_to(message: &Element, owner: &str) -> bool {
    message.attr("to").is_none_or(|to| {
        to.parse::<Jid>()
            .ok()
            .is_some_and(|jid| jid.to_bare().as_str() == owner)
    })
}
fn bounded(xml: &Element) -> Result<()> {
    crate::crypto::bundle::xml(xml, MAX_XML)
        .map(|_| ())
        .map_err(|_| Error::Invalid)
}
fn attr(mut xml: Element, key: &str, value: impl Into<String>) -> Element {
    xml.set_attr("".into(), key.try_into().unwrap(), value.into());
    xml
}
fn el(name: &str, namespace: &str) -> Element {
    Element::builder(name, namespace).build()
}
fn child<'a>(xml: &'a Element, name: &str, namespace: &str) -> Result<Option<&'a Element>> {
    let mut children = xml.children().filter(|c| c.is(name, namespace));
    let value = children.next();
    if children.next().is_some() {
        return Err(Error::Invalid);
    }
    Ok(value)
}

/// A multi-session nickname may disclose one item per resource (XEP-0045 §19.2).
/// Collapse those resources only when they agree on identity and permissions.
fn user_item(user: &Element) -> Result<Option<&Element>> {
    const USER: &str = "http://jabber.org/protocol/muc#user";
    let mut items = user.children().filter(|item| item.is("item", USER));
    let Some(first) = items.next() else {
        return Ok(None);
    };
    let mut count = 1;
    for item in items {
        count += 1;
        if count > 128
            || ["role", "affiliation", "nick"]
                .iter()
                .any(|key| first.attr(*key) != item.attr(*key))
        {
            return Err(Error::Invalid);
        }
        let identity = |item: &Element| -> Result<String> {
            let jid = item
                .attr("jid")
                .ok_or(Error::Invalid)?
                .parse::<Jid>()
                .map_err(|_| Error::Invalid)?;
            if jid.node().is_none() {
                return Err(Error::Invalid);
            }
            Ok(jid.to_bare().to_string())
        };
        if identity(first)? != identity(item)? {
            return Err(Error::Invalid);
        }
    }
    Ok(Some(first))
}

#[cfg(test)]
mod fix_tests {
    use super::*;

    #[test]
    fn each_room_failure_text_leads_to_the_place_that_deals_with_it() {
        assert_eq!(fix_for(&Error::NotPrivate.to_string()), Some(Fix::Security));
        assert_eq!(fix_for(&Error::OccupantNotListed("lurker".into()).to_string()), Some(Fix::Participants));
        assert_eq!(
            Error::NotPrivate.to_string(),
            "This room is not members-only and non-anonymous, so it cannot be end-to-end encrypted"
        );
        for other in [Error::RecipientsUnavailable, Error::NotJoined, Error::Forbidden, Error::Invalid] {
            assert_eq!(fix_for(&other.to_string()), None, "{other:?}");
        }
        assert_eq!(fix_for("The server said: lurker is in this room"), None, "lookalike text");
    }
}
