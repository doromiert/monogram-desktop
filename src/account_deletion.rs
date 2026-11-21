//! Deleting an account on its server (XEP-0077 cancel registration), guarded by a confirmation
//! the user has to type: their full address, so an account cannot be deleted by a stray click.
//!
//! This module holds the parts that need no display: whether the typed text confirms, the stanza
//! payload, and the wording of each failure. `Ui::delete_account` (in `dialogs.rs`) drives them.
use crate::iq::Error;
use tokio_xmpp::minidom::Element;

pub use crate::iq::REGISTER_NS;

/// Whether `typed` is the full address `owner`. Surrounding whitespace is forgiven, and so is case
/// (addresses are case-insensitive), but nothing else: not the user name, not the domain alone,
/// not an address with extra text. An empty `owner` is never confirmed.
pub fn confirmation_matches(owner: &str, typed: &str) -> bool {
    let typed = typed.trim();
    !owner.is_empty() && typed.eq_ignore_ascii_case(owner)
}

/// `<query xmlns='jabber:iq:register'><remove/></query>`: ask the server to delete the account
/// this connection is signed in to.
pub fn cancel_registration() -> Element {
    Element::builder("query", REGISTER_NS)
        .append(Element::builder("remove", REGISTER_NS).build())
        .build()
}

/// What to tell the user when the server did not confirm the deletion. Nothing is removed locally
/// for any of these: the account may still exist.
pub fn failure_notice(error: &Error) -> String {
    match error {
        Error::Rejected { condition, .. } => match condition.as_str() {
            "forbidden" | "not-allowed" | "service-unavailable" | "feature-not-implemented" => {
                "This server does not let you delete the account from an app. Ask its administrator, or use the server's own website.".into()
            }
            "not-authorized" => {
                "The server did not accept the request. Sign in again, then retry.".into()
            }
            _ => "The server refused to delete the account. It has not been removed.".into(),
        },
        // The server often closes the stream as soon as it deletes the account, so a dropped
        // connection is not proof either way.
        Error::Closed => {
            "The connection closed before the server answered. The account may or may not be deleted; sign in again to check. Nothing was removed from this device.".into()
        }
        Error::Timeout => {
            "The server did not answer in time. The account has not been removed from this device; try again.".into()
        }
        Error::Invalid | Error::Busy => {
            "The server's answer could not be used. The account has not been removed.".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_full_address_confirms() {
        let owner = "bob@example.com";
        assert!(confirmation_matches(owner, "bob@example.com"));
        assert!(confirmation_matches(owner, "  bob@example.com\n"));
        assert!(confirmation_matches(owner, "Bob@Example.COM"));
        for typed in [
            "",
            "   ",
            "bob",
            "example.com",
            "@example.com",
            "bob@",
            "bob@example",
            "bob@example.com.",
            "bob@example.com/phone",
            "xbob@example.com",
            "bob@example.comx",
            "bob @example.com",
            "alice@example.com",
            "bob@example.org",
            "delete",
        ] {
            assert!(!confirmation_matches(owner, typed), "{typed:?} must not confirm");
        }
    }

    #[test]
    fn nothing_confirms_an_empty_account() {
        assert!(!confirmation_matches("", ""));
        assert!(!confirmation_matches("", "  "));
        assert!(!confirmation_matches("", "bob@example.com"));
    }

    #[test]
    fn lookalike_addresses_do_not_confirm() {
        // A Cyrillic "о" is not an ASCII "o": case folding must stay ASCII-only.
        assert!(!confirmation_matches("bob@example.com", "b\u{43e}b@example.com"));
        assert!(!confirmation_matches("bob@example.com", "bob@example.com\u{200b}"));
    }

    #[test]
    fn cancel_registration_is_a_bare_remove_request() {
        let query = cancel_registration();
        assert!(query.is("query", REGISTER_NS));
        let children: Vec<_> = query.children().collect();
        assert_eq!(children.len(), 1, "no form, user name or password is sent");
        assert!(children[0].is("remove", REGISTER_NS));
        assert_eq!(children[0].children().count(), 0);
        assert_eq!(query.attr("to"), None);
    }

    #[test]
    fn every_failure_says_nothing_was_removed_or_that_it_is_unknown() {
        let rejected = |condition: &str| Error::Rejected {
            condition: condition.into(),
            pubsub: None,
        };
        for error in [
            rejected("forbidden"),
            rejected("not-allowed"),
            rejected("service-unavailable"),
            rejected("not-authorized"),
            rejected("internal-server-error"),
            Error::Closed,
            Error::Timeout,
            Error::Invalid,
            Error::Busy,
        ] {
            let notice = failure_notice(&error);
            assert!(!notice.is_empty() && notice.len() < 400, "{error:?}: {notice}");
            assert!(
                notice.contains("not") || notice.contains("Nothing"),
                "{error:?} must say the local account stays: {notice}"
            );
        }
    }

    #[test]
    fn server_policy_refusals_point_to_the_administrator() {
        for condition in ["forbidden", "not-allowed", "service-unavailable"] {
            let notice = failure_notice(&Error::Rejected {
                condition: condition.into(),
                pubsub: None,
            });
            assert!(notice.contains("administrator"), "{condition}: {notice}");
        }
    }

    #[test]
    fn a_dropped_connection_is_reported_as_unknown_not_as_success() {
        let notice = failure_notice(&Error::Closed);
        assert!(notice.contains("may or may not"));
        assert!(notice.contains("Nothing was removed"));
    }
}
