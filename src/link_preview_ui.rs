// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{
    link_previews::{Kind, Preview, Thumbnail},
    ui::Ui,
    widgets::*,
};
use adw::prelude::*;
use gtk::glib;
use std::{cell::Cell, rc::Rc};

const ARTICLE_THUMBNAIL: i32 = 88;
const LARGE_THUMBNAIL_WIDTH: i32 = 320;
const LARGE_THUMBNAIL_HEIGHT: i32 = 220;

fn preview_picture(thumbnail: &Thumbnail, large: bool, video: bool) -> gtk::Widget {
    // This is a backend-produced, sandbox-decoded local file. Remote thumbnail
    // URLs must never reach GTK's native image loader.
    let picture = gtk::Picture::for_filename(&thumbnail.path);
    picture.add_css_class("photo-tile");
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_can_shrink(true);
    picture.set_alternative_text(Some("Link preview thumbnail"));
    if large {
        let ratio = if thumbnail.width == 0 || thumbnail.height == 0 {
            16. / 9.
        } else {
            f64::from(thumbnail.width) / f64::from(thumbnail.height)
        };
        let height = (f64::from(LARGE_THUMBNAIL_WIDTH) / ratio)
            .round()
            .clamp(120., f64::from(LARGE_THUMBNAIL_HEIGHT)) as i32;
        picture.set_size_request(LARGE_THUMBNAIL_WIDTH, height);
    } else {
        picture.set_size_request(ARTICLE_THUMBNAIL, ARTICLE_THUMBNAIL);
    }
    if !video {
        return picture.upcast();
    }
    let overlay = gtk::Overlay::builder().child(&picture).build();
    let play = gtk::Image::from_icon_name("media-playback-start-symbolic");
    play.set_pixel_size(if large { 34 } else { 24 });
    play.set_halign(gtk::Align::Center);
    play.set_valign(gtk::Align::Center);
    play.set_can_target(false);
    overlay.add_overlay(&play);
    overlay.upcast()
}

fn metadata(preview: &Preview) -> gtk::Box {
    let words = bx(true, 2);
    words.set_hexpand(true);
    words.set_valign(gtk::Align::Start);
    let site = if preview.site_name.trim().is_empty() {
        preview.display_url.trim()
    } else {
        preview.site_name.trim()
    };
    if !site.is_empty() {
        let site = text(site, "reply-author");
        site.set_ellipsize(gtk::pango::EllipsizeMode::End);
        site.set_max_width_chars(38);
        site.set_single_line_mode(true);
        words.append(&site);
    }
    if !preview.title.trim().is_empty() {
        let title = wrap(preview.title.trim(), "heading");
        title.set_lines(3);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(42);
        words.append(&title);
    }
    if !preview.description.trim().is_empty() {
        let description = wrap(preview.description.trim(), "reply-preview");
        description.set_lines(4);
        description.set_ellipsize(gtk::pango::EllipsizeMode::End);
        description.set_max_width_chars(42);
        words.append(&description);
    }
    words
}

/// Native preview surface. The validated destination is opened only by the
/// user's click; no player, iframe, or remote image is loaded here.
pub(crate) fn link_preview_card(preview: &Preview) -> gtk::LinkButton {
    let content = bx(true, 6);
    let large = !matches!(&preview.kind, Kind::Article);
    let video = matches!(&preview.kind, Kind::Video);
    let words = metadata(preview);
    if large {
        content.append(&words);
        if let Some(thumbnail) = preview.thumbnail.as_ref() {
            content.append(&preview_picture(thumbnail, true, video));
        }
    } else {
        let row = bx(false, 9);
        row.append(&words);
        if let Some(thumbnail) = preview.thumbnail.as_ref() {
            row.append(&preview_picture(thumbnail, false, false));
        }
        content.append(&row);
    }
    let button = gtk::LinkButton::builder()
        .uri(&preview.url)
        .child(&content)
        .build();
    button.add_css_class("flat");
    button.add_css_class("message-reply");
    button.add_css_class("link-preview");
    button.set_halign(gtk::Align::Start);
    button.set_hexpand(false);
    button.set_tooltip_text(Some(&preview.display_url));
    let accessible = [preview.site_name.trim(), preview.title.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let accessible = if accessible.is_empty() {
        format!("Open link to {}", preview.display_url)
    } else {
        format!("{accessible}. Open link")
    };
    button.update_property(&[gtk::accessible::Property::Label(&accessible)]);
    button
}

fn set_switch(row: &adw::SwitchRow, changing: &Cell<bool>, active: bool) {
    changing.set(true);
    row.set_active(active);
    changing.set(false);
}

#[derive(Clone)]
struct PermissionControls {
    automatic: glib::WeakRef<adw::SwitchRow>,
    encrypted: glib::WeakRef<adw::SwitchRow>,
    spinner: glib::WeakRef<gtk::Spinner>,
    automatic_changing: Rc<Cell<bool>>,
    encrypted_changing: Rc<Cell<bool>>,
    pending: Rc<Cell<bool>>,
}
impl PermissionControls {
    fn set_interactive(&self, interactive: bool) {
        if let Some(row) = self.automatic.upgrade() {
            row.set_sensitive(interactive);
        }
        if let Some(row) = self.encrypted.upgrade() {
            row.set_sensitive(interactive);
        }
    }
    fn set_pending(&self, pending: bool) {
        self.pending.set(pending);
        self.set_interactive(!pending);
        if let Some(spinner) = self.spinner.upgrade() {
            spinner.set_visible(pending);
            spinner.set_spinning(pending);
        }
    }
    fn sync(&self, ui: &Ui) {
        let (enabled, encrypted) = ui.link_preview_permissions();
        if let Some(row) = self.automatic.upgrade() {
            set_switch(&row, &self.automatic_changing, enabled);
        }
        if let Some(row) = self.encrypted.upgrade() {
            set_switch(&row, &self.encrypted_changing, encrypted);
        }
    }
}

fn same_owner(ui: &Ui, owner: &str, _generation: u64) -> bool {
    ui.state.borrow().account == owner
}

fn begin_permission_update(
    ui: Rc<Ui>,
    controls: PermissionControls,
    owner: String,
    generation: u64,
    enabled: bool,
    encrypted: bool,
) {
    if controls.pending.replace(true) {
        return;
    }
    controls.set_pending(true);
    // Construct the owned future first, then release the strong UI reference.
    // The SQLite transaction and queue transition stay outside the UI thread.
    let update = ui.set_link_preview_permissions(enabled, encrypted);
    let weak = Rc::downgrade(&ui);
    drop(ui);
    glib::MainContext::default().spawn_local(async move {
        let result = update.await;
        if let Some(ui) = weak
            .upgrade()
            .filter(|ui| same_owner(ui, &owner, generation))
        {
            // The getter is authoritative after both success and failure. This
            // also rolls the optimistic switch state back on a durable error.
            controls.sync(&ui);
            if let Err(error) = result {
                ui.notice(&error);
            }
        }
        controls.set_pending(false);
    });
}

fn consent(
    ui: &Rc<Ui>,
    row: &adw::SwitchRow,
    controls: PermissionControls,
    owner: String,
    generation: u64,
    enabled: bool,
    encrypted: bool,
    encrypted_only: bool,
) {
    // The switch never displays an enabled state before durable consent lands.
    let changing = if encrypted_only {
        &controls.encrypted_changing
    } else {
        &controls.automatic_changing
    };
    set_switch(row, changing, false);
    // Serialize consent with the sibling permission. Otherwise confirming an
    // older dialog could overwrite a setting changed while it was open.
    controls.set_interactive(false);
    let dialog = adw::AlertDialog::builder()
        .heading(if encrypted_only {
            "Allow Link Embeds in Encrypted Chats?"
        } else {
            "Enable Automatic Link Embeds?"
        })
        .body("The linked destination and thumbnail host can see your IP address and the full requested URL, including its query. Preview metadata stays on this device and is never added to the XMPP message.")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("enable", "Enable")]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("enable", adw::ResponseAppearance::Suggested);
    let weak = Rc::downgrade(ui);
    let cancelled = controls.clone();
    dialog.connect_response(Some("cancel"), move |_, _| {
        cancelled.set_interactive(true);
    });
    dialog.connect_response(Some("enable"), move |_, _| {
        let Some(ui) = weak.upgrade() else {
            controls.set_interactive(true);
            return;
        };
        if !same_owner(&ui, &owner, generation) {
            controls.set_interactive(true);
            return;
        }
        begin_permission_update(
            ui,
            controls.clone(),
            owner.clone(),
            generation,
            enabled,
            encrypted,
        );
    });
    dialog.present(Some(&ui.window));
}

impl Ui {
    /// Account-scoped controls. Registration is intentionally deferred until
    /// the root worker lands the cache-backed permission hooks.
    pub(crate) fn link_preview_preferences(self: &Rc<Self>) -> adw::PreferencesGroup {
        let (enabled, encrypted) = self.link_preview_permissions();
        let group = adw::PreferencesGroup::builder()
            .title("Link Previews")
            .description(
                "Previews are fetched directly from the linked website and cached only on this device.",
            )
            .build();
        let automatic = adw::SwitchRow::builder()
            .title("Automatic Link Embeds")
            .subtitle("Show a native preview for the first HTTPS link in a message")
            .active(enabled)
            .build();
        let encrypted_row = adw::SwitchRow::builder()
            .title("Allow in Encrypted Chats")
            .subtitle("Separate permission for links revealed after decryption")
            .active(encrypted)
            .build();
        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        spinner.set_tooltip_text(Some("Saving link preview permissions"));
        automatic.add_suffix(&spinner);

        let generation = self.generation.get();
        let owner = self.state.borrow().account.clone();
        let controls = PermissionControls {
            automatic: automatic.downgrade(),
            encrypted: encrypted_row.downgrade(),
            spinner: spinner.downgrade(),
            automatic_changing: Rc::new(Cell::new(false)),
            encrypted_changing: Rc::new(Cell::new(false)),
            pending: Rc::new(Cell::new(false)),
        };
        let weak = Rc::downgrade(self);
        let guard = controls.clone();
        let account = owner.clone();
        automatic.connect_active_notify(move |row| {
            if guard.automatic_changing.get() || guard.pending.get() {
                return;
            }
            let Some(ui) = weak
                .upgrade()
                .filter(|ui| same_owner(ui, &account, generation))
            else {
                return;
            };
            let (current, encrypted) = ui.link_preview_permissions();
            if row.is_active() == current {
                return;
            }
            if row.is_active() {
                consent(
                    &ui,
                    row,
                    guard.clone(),
                    account.clone(),
                    generation,
                    true,
                    encrypted,
                    false,
                );
            } else {
                begin_permission_update(
                    ui,
                    guard.clone(),
                    account.clone(),
                    generation,
                    false,
                    encrypted,
                );
            }
        });

        let weak = Rc::downgrade(self);
        let guard = controls.clone();
        encrypted_row.connect_active_notify(move |row| {
            if guard.encrypted_changing.get() || guard.pending.get() {
                return;
            }
            let Some(ui) = weak
                .upgrade()
                .filter(|ui| same_owner(ui, &owner, generation))
            else {
                return;
            };
            let (enabled, current) = ui.link_preview_permissions();
            if row.is_active() == current {
                return;
            }
            if row.is_active() {
                consent(
                    &ui,
                    row,
                    guard.clone(),
                    owner.clone(),
                    generation,
                    enabled,
                    true,
                    true,
                );
            } else {
                begin_permission_update(
                    ui,
                    guard.clone(),
                    owner.clone(),
                    generation,
                    enabled,
                    false,
                );
            }
        });

        group.add(&automatic);
        group.add(&encrypted_row);
        group
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        inline_emoji_ui::InlineMessageText,
        packs::{InlineAsset, InlineEmoji, InlineRange},
    };

    fn labels(widget: &gtk::Widget, found: &mut Vec<gtk::Label>) {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            found.push(label.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            labels(&current, found);
            child = current.next_sibling();
        }
    }

    fn pictures(widget: &gtk::Widget, found: &mut Vec<gtk::Picture>) {
        if let Some(picture) = widget.downcast_ref::<gtk::Picture>() {
            found.push(picture.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            pictures(&current, found);
            child = current.next_sibling();
        }
    }

    #[test]
    fn native_links_selection_and_preview_clamp() {
        gtk::init().unwrap();
        let source = "😀 Open example.org/a?q=1 and https://second.example/path.";
        let links =
            crate::link_previews::clickable_urls(source, Some(&crate::spoilers::Context::clear()));
        assert_eq!(links.len(), 2);

        let body = wrap(source, "message-text");
        let status = bx(false, 2);
        status.append(&label("12:34", "message-meta"));
        let message = MessageText::with_links(&body, &status, links.clone());
        assert_eq!(body.text(), format!("{source} \u{fffc}"));
        assert!(
            body.label()
                .contains("<a href=\"https://example.org/a?q=1\"")
        );
        assert!(
            body.label()
                .contains("<a href=\"https://second.example/path\"")
        );
        body.select_region(0, source.chars().count() as i32);
        assert_eq!(
            body.selection_bounds(),
            Some((0, source.chars().count() as i32))
        );
        message.refresh_shape();
        assert!(
            body.attributes()
                .is_some_and(|attrs| attrs.attributes().len() >= 3)
        );

        let plain = wrap(source, "message-text");
        linkify_message_label(&plain, source, &links);
        assert_eq!(plain.text(), source);
        assert!(plain.is_selectable());

        let asset = InlineAsset::new("image/png".into(), vec![1]).unwrap();
        let emoji = InlineEmoji::new(
            source.into(),
            vec![InlineRange {
                start: 0,
                end: 1,
                asset: asset.clone(),
            }],
        )
        .unwrap();
        let bytes = glib::Bytes::from_owned(vec![0x55; 20 * 20 * 4]);
        let texture =
            gtk::gdk::MemoryTexture::new(20, 20, gtk::gdk::MemoryFormat::R8g8b8a8, &bytes, 20 * 4);
        let inline_status = bx(false, 2);
        inline_status.append(&label("12:34", "message-meta"));
        let linked_inline = InlineMessageText::new(source, &emoji, Some(&inline_status), &links);
        linked_inline.set_texture(&asset.cid, texture.clone().upcast());
        let inline_message = linked_inline
            .root
            .first_child()
            .and_downcast::<MessageText>()
            .unwrap();
        inline_message.set_compact(true);
        let mut inline_labels = Vec::new();
        labels(linked_inline.root.upcast_ref(), &mut inline_labels);
        assert!(inline_labels[0].label().contains("<a href="));
        assert!(
            inline_labels[0]
                .attributes()
                .is_some_and(|attrs| attrs.attributes().len() >= 4)
        );
        let mut inline_pictures = Vec::new();
        pictures(linked_inline.root.upcast_ref(), &mut inline_pictures);
        assert!(inline_pictures.iter().any(gtk::Picture::is_visible));

        let preview_inline = InlineMessageText::new(source, &emoji, None, &links);
        preview_inline.set_texture(&asset.cid, texture.upcast());
        let mut preview_labels = Vec::new();
        labels(preview_inline.root.upcast_ref(), &mut preview_labels);
        assert!(preview_labels[0].label().contains("<a href="));
        let mut preview_pictures = Vec::new();
        pictures(preview_inline.root.upcast_ref(), &mut preview_pictures);
        assert!(preview_pictures.iter().any(gtk::Picture::is_visible));

        let preview = Preview {
            url: "https://example.org/a?q=1".into(),
            display_url: "example.org/a?q=1".into(),
            site_name: "Example".into(),
            title: "A deliberately long cached title that must stay inside the native card clamp even when the message bubble is narrow".into(),
            description: "A deliberately long cached description that must stay inside four native lines. It remains metadata below the message text and never replaces the clickable source URL.".into(),
            kind: Kind::Article,
            thumbnail: None,
            media: None,
        };
        let card = link_preview_card(&preview);
        assert_eq!(card.uri(), preview.url);
        let mut card_labels = Vec::new();
        labels(card.upcast_ref(), &mut card_labels);
        assert!(
            card_labels
                .iter()
                .any(|label| label.text() == preview.title && label.lines() == 3)
        );
        assert!(
            card_labels
                .iter()
                .any(|label| label.text() == preview.description && label.lines() == 4)
        );
    }
}
