//! Making text that other people control safe to show in the app's own chrome.
//!
//! Nicknames, server error messages and similar strings come from other users or servers. They
//! are always shown as plain text (never markup), but they could still contain invisible or
//! direction-changing characters that reorder or hide what follows, or look like something the
//! app itself says ("(You)", "The server said…"). These helpers remove that.

/// Characters that change or hide how text reads without adding anything visible. The zero-width
/// joiner and non-joiner are kept: emoji sequences and some scripts need them.
fn invisible(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}' | '\u{200e}' | '\u{200f}'
            | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}'
            | '\u{feff}' | '\u{fff9}'..='\u{fffb}')
}

/// `text` with invisible and control characters removed, whitespace collapsed to single spaces,
/// and at most `max_chars` characters (ending in "…" when cut).
pub fn visible(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for c in text.chars().filter(|c| !invisible(*c) || c.is_whitespace()) {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    if out.chars().count() > max_chars {
        let mut cut: String = out.chars().take(max_chars.saturating_sub(1)).collect();
        cut.push('…');
        return cut;
    }
    out
}

/// A participant name for display. Another person's name can never read "(You)": the app marks
/// the user's own entry itself, so lookalikes in other names are shown in square brackets.
pub fn display_nick(nick: &str, own: bool) -> String {
    let clean = visible(nick, 64);
    if own {
        return clean;
    }
    // ASCII lowercasing keeps byte offsets, so the match positions index the original text.
    let lower = clean.to_ascii_lowercase();
    let mut out = String::with_capacity(clean.len());
    let mut at = 0;
    while let Some(found) = lower[at..].find("(you)") {
        let start = at + found;
        out.push_str(&clean[at..start]);
        out.push('[');
        out.push_str(&clean[start + 1..start + 4]);
        out.push(']');
        at = start + 5;
    }
    out.push_str(&clean[at..]);
    out
}

/// The toast for a message that could not be sent. A reason that came from a server says so, so
/// a server cannot pass its own words off as the app's.
pub fn failed_send_toast(reason: &str) -> String {
    let reason = visible(reason, 100);
    if reason.is_empty() {
        "Message could not be sent. Retry from its menu.".into()
    } else {
        format!("Message not sent: {reason}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_and_direction_characters_are_removed_but_emoji_joiners_stay() {
        assert_eq!(visible("a\u{202e}bc\u{200b}d\u{0}e\u{7}", 50), "abcde");
        assert_eq!(visible("evil\u{202e}\u{2067}name", 50), "evilname");
        assert_eq!(visible("👨\u{200d}👩\u{200d}👧", 50), "👨\u{200d}👩\u{200d}👧", "a family emoji is one emoji");
        assert_eq!(visible("two\nlines\tand   gaps", 50), "two lines and gaps");
        assert_eq!(visible("  \n ", 50), "");
    }

    #[test]
    fn text_is_cut_to_the_limit_with_an_ellipsis() {
        assert_eq!(visible("abcdef", 6), "abcdef");
        assert_eq!(visible("abcdefg", 6), "abcde…");
        assert_eq!(visible(&"x".repeat(1_000_000), 64).chars().count(), 64);
        assert_eq!(visible("ééééé", 3), "éé…", "counts characters, not bytes");
    }

    #[test]
    fn another_persons_name_cannot_say_you() {
        assert_eq!(display_nick("Romeo", true), "Romeo");
        assert_eq!(display_nick("Romeo (You)", false), "Romeo [You]");
        assert_eq!(display_nick("admin (YOU) (you)", false), "admin [YOU] [you]");
        assert_eq!(display_nick("(you)", false), "[you]");
        assert_eq!(display_nick("your (youth)", false), "your (youth)", "only the exact marker");
        // Hidden characters cannot be used to slip past the check.
        assert_eq!(display_nick("(y\u{200b}ou)", false), "[you]");
        assert_eq!(display_nick("İstanbul (You)", false), "İstanbul [You]", "non-ASCII letters do not shift the match");
    }

    #[test]
    fn failed_send_toasts_are_plain_bounded_and_fall_back_when_empty() {
        assert_eq!(failed_send_toast(""), "Message could not be sent. Retry from its menu.");
        assert_eq!(failed_send_toast("\u{202e}  "), "Message could not be sent. Retry from its menu.");
        assert_eq!(failed_send_toast("the server said forbidden"), "Message not sent: the server said forbidden");
        let long = failed_send_toast(&"a".repeat(5000));
        assert_eq!(long.chars().count(), "Message not sent: ".chars().count() + 100);
        assert!(!failed_send_toast("x\u{202e}y\nz").contains(['\u{202e}', '\n']));
    }
}
