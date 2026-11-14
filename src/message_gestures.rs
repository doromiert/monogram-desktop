//! Desktop equivalents of mobile's incoming/outgoing double-tap preferences.
use crate::ui::Ui;
use adw::prelude::*;
use std::rc::Rc;

const ACTIONS: &[(&str, &str, &str)] = &[
    ("none", "Disabled", "process-stop-symbolic"),
    ("reaction", "Reaction", "emote-love-symbolic"),
    ("reply", "Reply", "mail-reply-sender-symbolic"),
    ("copy", "Copy", "edit-copy-symbolic"),
    ("forward", "Forward", "mail-forward-symbolic"),
    ("edit", "Edit", "document-edit-symbolic"),
    ("delete", "Delete", "user-trash-symbolic"),
    ("details", "Message Details", "dialog-information-symbolic"),
];
fn key(own: bool) -> &'static str {
    if own {
        "double_click_own"
    } else {
        "double_click_incoming"
    }
}
impl Ui {
    pub(crate) fn double_click_message(self: &Rc<Self>, id: &str) -> bool {
        let Some(message) = self.state.borrow().message(id).cloned() else {
            return false;
        };
        if message.retracted || message.unavailable || !self.selected.borrow().is_empty() {
            return false;
        }
        match self
            .store
            .pref(key(message.own), if message.own { "edit" } else { "reply" })
            .as_str()
        {
            "reply" => {
                self.replying.replace(Some(id.into()));
                self.update_reply();
                self.composer.grab_focus();
                self.save_draft();
            }
            "edit" if message.own && message.kind == "text" => self.edit_message(id),
            "reaction" => self.react(id, &self.store.pref("double_click_reaction", "❤️")),
            "copy" if message.attachment.is_some() => self.copy_attachment(id),
            "copy" => self.window.clipboard().set_text(&message.content_text()),
            "forward" => self.forward(vec![message]),
            "delete" => self.delete_confirmation(vec![id.into()]),
            "details" => self.message_details(&message),
            _ => return false,
        }
        true
    }
    pub(crate) fn add_double_click_preferences(self: &Rc<Self>, page: &adw::PreferencesPage) {
        let group = adw::PreferencesGroup::builder().title("Double-click actions")
            .description("Choose an action for each kind of message. Actions apply when the message supports them. Deleting asks for confirmation.").build();
        let preview = gtk::Box::new(gtk::Orientation::Vertical, 18);
        preview.add_css_class("double-click-preview");
        let mut icons = Vec::new();
        for own in [false, true] {
            let icon = gtk::Image::new();
            icon.set_pixel_size(30);
            let bubble = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            bubble.add_css_class("double-click-preview-bubble");
            bubble.add_css_class(if own { "outgoing" } else { "incoming" });
            bubble.set_size_request(160, 72);
            bubble.set_halign(if own {
                gtk::Align::End
            } else {
                gtk::Align::Start
            });
            icon.set_hexpand(true);
            bubble.append(&icon);
            preview.append(&bubble);
            icons.push(icon);
        }
        group.add(&preview);
        for (own, icon) in [false, true].into_iter().zip(icons) {
            let options = ACTIONS
                .iter()
                .filter(|(id, _, _)| own || *id != "edit")
                .copied()
                .collect::<Vec<_>>();
            let labels = options
                .iter()
                .map(|(_, label, _)| *label)
                .collect::<Vec<_>>();
            let saved = self
                .store
                .pref(key(own), if own { "edit" } else { "reply" });
            let selected = options
                .iter()
                .position(|(id, _, _)| *id == saved)
                .unwrap_or(0);
            icon.set_icon_name(Some(options[selected].2));
            let row = adw::ComboRow::builder()
                .title(if own {
                    "Your messages"
                } else {
                    "Incoming messages"
                })
                .model(&gtk::StringList::new(&labels))
                .selected(selected as u32)
                .build();
            let weak = Rc::downgrade(self);
            row.connect_selected_notify(move |row| {
                if let Some(ui) = weak.upgrade() {
                    if let Some((action, _, name)) = options.get(row.selected() as usize) {
                        let _ = ui.store.set_pref(key(own), action);
                        icon.set_icon_name(Some(name));
                    }
                }
            });
            group.add(&row);
        }
        let saved = self.store.pref("double_click_reaction", "❤️");
        let mut emojis = self
            .store
            .emoji_preferences()
            .unwrap_or_default()
            .quick_reactions(&[]);
        if !emojis.contains(&saved) {
            emojis.insert(0, saved.clone());
        }
        let selected = emojis.iter().position(|emoji| emoji == &saved).unwrap_or(0);
        let refs = emojis.iter().map(String::as_str).collect::<Vec<_>>();
        let quick = adw::ComboRow::builder()
            .title("Quick Reaction")
            .subtitle("Emoji used when a double-click action is Reaction")
            .model(&gtk::StringList::new(&refs))
            .selected(selected as u32)
            .build();
        let weak = Rc::downgrade(self);
        quick.connect_selected_notify(move |row| {
            if let Some(ui) = weak.upgrade() {
                if let Some(emoji) = emojis.get(row.selected() as usize) {
                    let _ = ui.store.set_pref("double_click_reaction", emoji);
                }
            }
        });
        group.add(&quick);
        page.add(&group);
    }
}
