//! TextView completion using GTK's shipped emoji data and native emoji chooser.
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::OnceLock,
};

#[derive(Clone)]
pub struct EmojiEntry {
    pub text: String,
    pub name: String,
    pub group: u32,
    terms: Vec<String>,
}
static EMOJI: OnceLock<Vec<EmojiEntry>> = OnceLock::new();

fn normalized(value: &str) -> String {
    value.to_lowercase().replace(['_', '-'], " ")
}
fn index() -> &'static Vec<EmojiEntry> {
    EMOJI.get_or_init(|| {
        // GTK 4's gtkemojichooser.c defines this resource and its GVariant schema.
        let Ok(bytes) = gio::resources_lookup_data(
            "/org/gtk/libgtk/emoji/en.data",
            gio::ResourceLookupFlags::NONE,
        ) else {
            return Vec::new();
        };
        type Item = (Vec<u32>, String, String, Vec<String>, Vec<String>, u32);
        let data = glib::Variant::from_bytes::<Vec<Item>>(&bytes);
        let Some(items) = data.get::<Vec<Item>>() else {
            return Vec::new();
        };
        items
            .into_iter()
            .filter_map(|(codes, name, translation, keywords, annotations, group)| {
                let text: String = codes
                    .into_iter()
                    .filter_map(|code| match code {
                        0 => Some('\u{fe0f}'),
                        0x1f3fb => None,
                        code => char::from_u32(code),
                    })
                    .collect();
                if text.is_empty() {
                    return None;
                }
                let terms = std::iter::once(name.as_str())
                    .chain(std::iter::once(translation.as_str()))
                    .chain(keywords.iter().map(String::as_str))
                    .chain(annotations.iter().map(String::as_str))
                    .map(normalized)
                    .collect();
                Some(EmojiEntry {
                    text,
                    name,
                    group,
                    terms,
                })
            })
            .collect()
    })
}

pub fn catalog() -> &'static [EmojiEntry] {
    index().as_slice()
}

pub fn search(query: &str) -> Vec<EmojiEntry> {
    let query = normalized(query);
    let words: Vec<_> = query.split_whitespace().collect();
    index()
        .iter()
        .filter(|emoji| {
            words.iter().all(|word| {
                emoji.terms.iter().any(|term| term.contains(word)) || emoji.text.contains(word)
            })
        })
        .cloned()
        .collect()
}

#[derive(Clone, PartialEq, Eq)]
struct Token {
    start: i32,
    end: i32,
    query: String,
}
fn token(view: &gtk::TextView) -> Option<Token> {
    let buffer = view.buffer();
    if buffer.selection_bounds().is_some() {
        return None;
    }
    let end = buffer.cursor_position();
    let first = (end - 64).max(0);
    let text = buffer.text(
        &buffer.iter_at_offset(first),
        &buffer.iter_at_offset(end),
        false,
    );
    let colon = text.rfind(':')?;
    let before = &text[..colon];
    if before
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || matches!(c, ':' | '/' | '_'))
    {
        return None;
    }
    if colon == 0 && first != 0 {
        return None;
    }
    let query = &text[colon + 1..];
    if query.is_empty()
        || !query
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '+'))
    {
        return None;
    }
    Some(Token {
        start: first + before.chars().count() as i32,
        end,
        query: normalized(query),
    })
}

pub struct Completion {
    view: glib::WeakRef<gtk::TextView>,
    popover: gtk::Popover,
    list: gtk::ListBox,
    token: RefCell<Option<Token>>,
    matches: RefCell<Vec<usize>>,
    selected: Cell<usize>,
}
impl Completion {
    fn hide(&self) {
        self.popover.popdown();
        self.token.borrow_mut().take();
    }
    fn update(&self) {
        let Some(view) = self.view.upgrade().filter(|view| view.has_focus()) else {
            self.hide();
            return;
        };
        let Some(token) = token(&view) else {
            self.hide();
            return;
        };
        if self.token.borrow().as_ref() == Some(&token) && self.popover.is_mapped() {
            return;
        }
        let Some(data) = EMOJI.get() else {
            self.hide();
            return;
        };
        let mut matches: Vec<_> = data
            .iter()
            .enumerate()
            .filter_map(|(index, emoji)| {
                let name = &emoji.terms[0];
                let score = if name == &token.query {
                    0
                } else if name.starts_with(&token.query) {
                    1
                } else if emoji
                    .terms
                    .iter()
                    .any(|term| term.starts_with(&token.query))
                {
                    2
                } else if emoji.terms.iter().any(|term| {
                    term.split_whitespace()
                        .any(|word| word.starts_with(&token.query))
                }) {
                    3
                } else {
                    return None;
                };
                Some((score, index))
            })
            .collect();
        matches.sort_by_key(|&(score, index)| (score, index));
        matches.truncate(6);
        if matches.is_empty() {
            self.hide();
            return;
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let matches: Vec<_> = matches.into_iter().map(|(_, index)| index).collect();
        for index in &matches {
            let emoji = &data[*index];
            let row = gtk::ListBoxRow::builder()
                .focusable(false)
                .focus_on_click(false)
                .build();
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            for (value, class) in [(&emoji.text, "title-2"), (&emoji.name, "")] {
                let label = gtk::Label::new(Some(value));
                label.set_xalign(0.);
                if !class.is_empty() {
                    label.add_css_class(class);
                }
                line.append(&label);
            }
            line.set_margin_top(5);
            line.set_margin_bottom(5);
            line.set_margin_start(10);
            line.set_margin_end(10);
            row.set_child(Some(&line));
            self.list.append(&row);
        }
        self.matches.replace(matches);
        self.selected.set(0);
        self.list.select_row(self.list.row_at_index(0).as_ref());
        let location = view.iter_location(&view.buffer().iter_at_offset(token.end));
        let (x, y) =
            view.buffer_to_window_coords(gtk::TextWindowType::Widget, location.x(), location.y());
        if self.popover.parent().is_none() {
            self.popover.set_parent(&view);
        }
        self.popover.set_pointing_to(Some(&gdk::Rectangle::new(
            x,
            y,
            1,
            location.height().max(1),
        )));
        self.token.replace(Some(token));
        self.popover.popup();
    }
    fn activate(&self, row: usize) {
        let Some(view) = self.view.upgrade() else {
            return;
        };
        let Some(expected) = self.token.borrow().clone() else {
            return;
        };
        if token(&view).as_ref() != Some(&expected) {
            self.hide();
            return;
        }
        let Some(index) = self.matches.borrow().get(row).copied() else {
            return;
        };
        let Some(emoji) = EMOJI.get().and_then(|data| data.get(index)) else {
            return;
        };
        self.hide();
        let buffer = view.buffer();
        buffer.begin_user_action();
        let mut start = buffer.iter_at_offset(expected.start);
        let mut end = buffer.iter_at_offset(expected.end);
        buffer.delete(&mut start, &mut end);
        buffer.insert(&mut start, &emoji.text);
        buffer.end_user_action();
        view.grab_focus();
    }
    pub fn key(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        if modifiers.contains(gdk::ModifierType::CONTROL_MASK) && key == gdk::Key::semicolon {
            self.hide();
            if let Some(view) = self.view.upgrade() {
                let _ = view.activate_action("misc.insert-emoji", None);
            }
            return true;
        }
        if !self.popover.is_mapped() || self.token.borrow().is_none() {
            return false;
        }
        match key {
            gdk::Key::Up | gdk::Key::Down => {
                let count = self.matches.borrow().len();
                let next = if key == gdk::Key::Up {
                    (self.selected.get() + count - 1) % count
                } else {
                    (self.selected.get() + 1) % count
                };
                self.selected.set(next);
                self.list
                    .select_row(self.list.row_at_index(next as i32).as_ref());
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab => {
                self.activate(self.selected.get())
            }
            gdk::Key::Escape => self.hide(),
            _ => return false,
        }
        true
    }
}

pub fn attach(view: &gtk::TextView) -> Rc<Completion> {
    view.set_input_hints(view.input_hints() | gtk::InputHints::EMOJI);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .focusable(false)
        .build();
    list.add_css_class("boxed-list");
    let popover = gtk::Popover::builder()
        .child(&list)
        .position(gtk::PositionType::Top)
        .autohide(false)
        .has_arrow(false)
        .build();
    let completion = Rc::new(Completion {
        view: view.downgrade(),
        popover,
        list,
        token: RefCell::new(None),
        matches: RefCell::new(Vec::new()),
        selected: Cell::new(0),
    });
    let state = completion.clone();
    view.buffer().connect_changed(move |_| state.update());
    let state = completion.clone();
    view.buffer()
        .connect_cursor_position_notify(move |_| state.update());
    let weak = Rc::downgrade(&completion);
    completion.list.connect_row_activated(move |_, row| {
        if let Some(state) = weak.upgrade() {
            state.activate(row.index() as usize);
        }
    });
    let focus = gtk::EventControllerFocus::new();
    let state = completion.clone();
    focus.connect_enter(move |_| state.update());
    let state = completion.clone();
    focus.connect_leave(move |_| state.hide());
    view.add_controller(focus);
    let mapped = Rc::downgrade(&completion);
    view.connect_map(move |_| {
        let mapped = mapped.clone();
        glib::idle_add_local_once(move || {
            if let Some(state) = mapped.upgrade() {
                state.update();
            }
        });
    });
    let popup = completion.popover.downgrade();
    view.connect_unmap(move |_| {
        if let Some(popup) = popup.upgrade() {
            popup.popdown();
            if popup.parent().is_some() {
                popup.unparent();
            }
        }
    });
    if EMOJI.get().is_none() {
        let weak = Rc::downgrade(&completion);
        glib::MainContext::default().spawn_local(async move {
            let _ = gio::spawn_blocking(index).await;
            if let Some(state) = weak.upgrade() {
                state.update();
            }
        });
    }
    completion
}

pub(crate) fn only_emojis(body: &str) -> bool {
    static SEQUENCES: OnceLock<std::collections::HashMap<char, Vec<String>>> = OnceLock::new();
    fn normalized_emoji(text: &str) -> String {
        text.chars()
            .filter(|character| {
                !character.is_whitespace()
                    && *character != '\u{fe0f}'
                    && !('\u{1f3fb}'..='\u{1f3ff}').contains(character)
            })
            .collect()
    }
    let sequences = SEQUENCES.get_or_init(|| {
        let mut by_start = std::collections::HashMap::<char, Vec<String>>::new();
        for entry in catalog() {
            let text = normalized_emoji(&entry.text);
            if let Some(first) = text.chars().next() {
                by_start.entry(first).or_default().push(text);
            }
        }
        for values in by_start.values_mut() {
            values.sort_by_key(|value| std::cmp::Reverse(value.len()));
            values.dedup();
        }
        by_start
    });
    let normalized = normalized_emoji(body);
    if normalized.is_empty() {
        return false;
    }
    let mut rest = normalized.as_str();
    while let Some(first) = rest.chars().next() {
        let Some(matched) = sequences
            .get(&first)
            .and_then(|values| values.iter().find(|value| rest.starts_with(value.as_str())))
        else {
            return false;
        };
        rest = &rest[matched.len()..];
    }
    true
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires a native GTK display for its emoji resource"]
    fn emoji_only_matches_sequences_without_matching_text() {
        gtk::init().unwrap();
        for body in ["😅✌️", "👍🏽\n❤️", "👨‍👩‍👧‍👦", "🇵🇱", "1️⃣"]
        {
            assert!(super::only_emojis(body), "{body:?}");
        }
        for body in ["", "  ", "hi 😅", "123", "#", "🏽", "/me 😅"] {
            assert!(!super::only_emojis(body), "{body:?}");
        }
    }
}
