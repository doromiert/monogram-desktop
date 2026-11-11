//! Live room bookmarks and XEP-0045 membership. A bookmark alone never authorizes sending.
pub mod archive;
mod bookmarks;
mod muc;

pub use bookmarks::{
    Bookmark, BookmarkInfo, bookmark_changed, load_bookmarks, subscribe_bookmarks,
};
pub use muc::{Presence, Rooms};

use crate::iq;
use std::fmt;
use tokio_xmpp::{jid::Jid, minidom::Element};
const CLIENT: &str = "jabber:client";
const MAX_XML: usize = 1024 * 1024;

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
    UnsupportedMessage,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "Invalid room address, nickname or server response",
            Self::Request(iq::Error::Timeout) => {
                "The server did not answer the room bookmark request"
            }
            Self::Request(iq::Error::Closed) => "Reconnect to synchronize room bookmarks",
            Self::Request(_) => "Could not synchronize room bookmarks",
            Self::Joining => "Waiting for the room to confirm membership",
            Self::AlreadyJoined => "You already joined this room",
            Self::NotJoined => "Join the room before sending messages",
            Self::Forbidden => "Your room role does not permit sending messages",
            Self::Locked => "The room requires configuration before you can send messages",
            Self::Encryption => {
                "Encrypted room sending is unavailable. Your encryption setting is unchanged."
            }
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
