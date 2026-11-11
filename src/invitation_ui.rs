//! Native presentation for durable contact requests and room invitations.
use crate::{rooms::Password, storage::RoomInvitation, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::glib;
use std::{cell::RefCell, rc::Rc};
use zeroize::Zeroizing;

fn status(label: &gtk::Label, message: &str, error: bool) {
    label.set_label(message);
    label.set_visible(!message.is_empty());
    if error {
        label.add_css_class("error");
    } else {
        label.remove_css_class("error");
    }
}

impl Ui {
    fn refresh_request_badge(&self) {
        let count = self.request_count();
        self.request_badge.set_label(&count.to_string());
        self.request_badge.set_visible(count > 0);
    }

    pub(crate) fn requests_and_invitations(self: &Rc<Self>) {
        let content = adw::Bin::new();
        let history = Rc::new(RefCell::new(Vec::new()));
        self.show_requests_page(&content, None, history);
        self.dialog("Requests and Invitations", &content, 520, 520);
    }

    fn show_requests_page(
        self: &Rc<Self>,
        content: &adw::Bin,
        before: Option<u64>,
        history: Rc<RefCell<Vec<Option<u64>>>>,
    ) {
        let page = adw::PreferencesPage::new();
        let pending = match self.pending_requests_before(before) {
            Ok(pending) => pending,
            Err(error) => {
                let group = adw::PreferencesGroup::builder()
                    .title("Requests and invitations")
                    .build();
                group.add(&wrap(&error, "error"));
                page.add(&group);
                content.set_child(Some(&scroll(&page)));
                return;
            }
        };

        let no_contacts = pending.contacts.is_empty();
        let no_rooms = pending.rooms.is_empty();
        let contact_cursor = pending.contact_cursor;
        let contact_total = pending.contact_total;
        if !no_contacts {
            let group = adw::PreferencesGroup::builder()
                .title("Contact requests")
                .description("Choose who may see your presence and contact-only profile data.")
                .build();
            for request in pending.contacts {
                let row = adw::ActionRow::builder()
                    .title(&request.peer)
                    .subtitle(&request.description)
                    .use_markup(false)
                    .activatable(true)
                    .build();
                let weak = Rc::downgrade(self);
                let peer = request.peer.clone();
                row.connect_activated(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        ui.contact_access(&peer);
                    }
                });
                let actions = bx(false, 6);
                actions.set_valign(gtk::Align::Center);
                let spinner = gtk::Spinner::builder()
                    .spinning(true)
                    .visible(false)
                    .build();
                let decline = gtk::Button::with_label("Decline");
                let accept_label = gtk::Label::builder()
                    .label("Accept and request back")
                    .wrap(true)
                    .max_width_chars(14)
                    .justify(gtk::Justification::Center)
                    .build();
                let accept = gtk::Button::builder().child(&accept_label).build();
                accept.add_css_class("suggested-action");
                actions.append(&spinner);
                actions.append(&decline);
                actions.append(&accept);
                row.add_suffix(&actions);
                let feedback = wrap("", "caption dim-label");
                feedback.set_visible(false);
                group.add(&row);
                group.add(&feedback);

                for (button, allow) in [(decline.clone(), false), (accept.clone(), true)] {
                    let weak = Rc::downgrade(self);
                    let request = request.clone();
                    let row = row.downgrade();
                    let feedback = feedback.downgrade();
                    let spinner = spinner.downgrade();
                    let decline = decline.downgrade();
                    let accept = accept.downgrade();
                    button.connect_clicked(move |_| {
                        let (
                            Some(ui),
                            Some(row),
                            Some(feedback),
                            Some(spinner),
                            Some(decline),
                            Some(accept),
                        ) = (
                            weak.upgrade(),
                            row.upgrade(),
                            feedback.upgrade(),
                            spinner.upgrade(),
                            decline.upgrade(),
                            accept.upgrade(),
                        )
                        else {
                            return;
                        };
                        decline.set_sensitive(false);
                        accept.set_sensitive(false);
                        spinner.set_visible(true);
                        status(&feedback, "Updating contact access…", false);
                        let weak = Rc::downgrade(&ui);
                        let row = row.downgrade();
                        let feedback = feedback.downgrade();
                        let spinner = spinner.downgrade();
                        let decline = decline.downgrade();
                        let accept = accept.downgrade();
                        let request = request.clone();
                        let operation = ui.respond_contact_request(&request, allow);
                        drop(ui);
                        glib::MainContext::default().spawn_local(async move {
                            let result = operation.await;
                            let (
                                Some(ui),
                                Some(row),
                                Some(feedback),
                                Some(spinner),
                                Some(decline),
                                Some(accept),
                            ) = (
                                weak.upgrade(),
                                row.upgrade(),
                                feedback.upgrade(),
                                spinner.upgrade(),
                                decline.upgrade(),
                                accept.upgrade(),
                            )
                            else {
                                return;
                            };
                            spinner.set_visible(false);
                            match result {
                                Ok(()) => {
                                    row.set_visible(false);
                                    feedback.set_visible(false);
                                    ui.refresh_request_badge();
                                }
                                Err(error) => {
                                    status(&feedback, &error, true);
                                    decline.set_sensitive(true);
                                    accept.set_sensitive(true);
                                }
                            }
                        });
                    });
                }
            }
            page.add(&group);
        }

        if before.is_some() || contact_cursor.is_some() {
            let navigation = adw::PreferencesGroup::new();
            let row = adw::ActionRow::builder()
                .title(format!(
                    "Showing up to 64 of {contact_total} contact requests"
                ))
                .build();
            let buttons = bx(false, 6);
            buttons.set_valign(gtk::Align::Center);
            if before.is_some() {
                let newer = gtk::Button::with_label("Newer");
                let weak = Rc::downgrade(self);
                let content = content.downgrade();
                let history = history.clone();
                newer.connect_clicked(move |_| {
                    let (Some(ui), Some(content)) = (weak.upgrade(), content.upgrade()) else {
                        return;
                    };
                    let next = history.borrow_mut().pop().unwrap_or(None);
                    ui.show_requests_page(&content, next, history.clone());
                });
                buttons.append(&newer);
            }
            if let Some(cursor) = contact_cursor {
                let older = gtk::Button::with_label("Older");
                let weak = Rc::downgrade(self);
                let content = content.downgrade();
                let history = history.clone();
                older.connect_clicked(move |_| {
                    let (Some(ui), Some(content)) = (weak.upgrade(), content.upgrade()) else {
                        return;
                    };
                    history.borrow_mut().push(before);
                    ui.show_requests_page(&content, Some(cursor), history.clone());
                });
                buttons.append(&older);
            }
            row.add_suffix(&buttons);
            navigation.add(&row);
            page.add(&navigation);
        }

        if before.is_none() && !no_rooms {
            let group = adw::PreferencesGroup::builder()
                .title("Room invitations")
                .description(
                    "Review an invitation before joining. Joining never grants fingerprint trust.",
                )
                .build();
            for invitation in pending.rooms {
                let subtitle = if invitation.reason.is_empty() {
                    format!("Invited by {}", invitation.inviter)
                } else {
                    format!("Invited by {}\n{}", invitation.inviter, invitation.reason)
                };
                let row = adw::ActionRow::builder()
                    .title(&invitation.room)
                    .subtitle(&subtitle)
                    .use_markup(false)
                    .activatable(true)
                    .build();
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                let weak = Rc::downgrade(self);
                let invitation = invitation.clone();
                let source = row.downgrade();
                row.connect_activated(move |_| {
                    if let (Some(ui), Some(source)) = (weak.upgrade(), source.upgrade()) {
                        ui.review_room_invitation(invitation.clone(), source);
                    }
                });
                group.add(&row);
            }
            page.add(&group);
        }

        if before.is_none() && no_contacts && no_rooms {
            let empty = adw::PreferencesGroup::new();
            empty.add(
                &adw::ActionRow::builder()
                    .title("No pending requests")
                    .subtitle("New contact requests and room invitations will appear here.")
                    .sensitive(false)
                    .build(),
            );
            page.add(&empty);
        }

        content.set_child(Some(&scroll(&page)));
    }

    fn review_room_invitation(self: &Rc<Self>, invitation: RoomInvitation, source: adw::ActionRow) {
        let page = adw::PreferencesPage::new();
        let summary = adw::PreferencesGroup::builder()
            .title("Room invitation")
            .build();
        let detail = if invitation.reason.is_empty() {
            format!("Invited by {}", invitation.inviter)
        } else {
            format!("Invited by {}\n{}", invitation.inviter, invitation.reason)
        };
        summary.add(
            &adw::ActionRow::builder()
                .title(&invitation.room)
                .subtitle(&detail)
                .use_markup(false)
                .build(),
        );
        page.add(&summary);
        let identity = adw::PreferencesGroup::builder()
            .title("Your room identity")
            .description("The password stays in the system keyring. Joining does not approve encryption fingerprints.")
            .build();
        let fallback = invitation
            .owner
            .split('@')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("Monogram");
        let nickname = adw::EntryRow::builder()
            .title("Your nickname")
            .text(fallback)
            .build();
        let password = adw::PasswordEntryRow::builder()
            .title("Room password")
            .build();
        identity.add(&nickname);
        identity.add(&password);
        page.add(&identity);

        let progress = adw::PreferencesGroup::new();
        let feedback = wrap("Loading invitation password…", "caption dim-label");
        progress.add(&feedback);
        page.add(&progress);

        let removal = adw::PreferencesGroup::new();
        let remove_row = adw::ActionRow::builder()
            .title("Dismiss invitation")
            .subtitle("Remove this saved invitation from this device.")
            .use_markup(false)
            .build();
        let dismiss = gtk::Button::with_label("Dismiss");
        dismiss.add_css_class("destructive-action");
        dismiss.set_valign(gtk::Align::Center);
        remove_row.add_suffix(&dismiss);
        removal.add(&remove_row);
        page.add(&removal);

        let (dialog, header) = self.dialog("Join Room", &scroll(&page), 520, 560);
        let busy = gtk::Spinner::builder()
            .spinning(true)
            .visible(false)
            .build();
        let join = gtk::Button::with_label("Join");
        join.add_css_class("suggested-action");
        join.set_sensitive(false);
        header.pack_end(&join);
        header.pack_end(&busy);

        let weak = Rc::downgrade(self);
        let password_target = password.downgrade();
        let feedback_target = feedback.downgrade();
        let join_target = join.downgrade();
        let invitation_for_secret = invitation.clone();
        let operation = self.invitation_password(&invitation_for_secret);
        glib::MainContext::default().spawn_local(async move {
            let result = operation.await;
            if weak.upgrade().is_none() {
                return;
            }
            let (Some(password), Some(feedback), Some(join)) = (
                password_target.upgrade(),
                feedback_target.upgrade(),
                join_target.upgrade(),
            ) else {
                return;
            };
            match result {
                Ok(Some(secret)) => {
                    password.set_text(&secret.0);
                    status(&feedback, "", false);
                }
                Ok(None) => status(&feedback, "", false),
                Err(error) => status(&feedback, &error, true),
            }
            join.set_sensitive(true);
        });

        let weak = Rc::downgrade(self);
        let dialog_target = dialog.downgrade();
        let source_target = source.downgrade();
        let feedback_target = feedback.downgrade();
        let busy_target = busy.downgrade();
        let join_target = join.downgrade();
        let nickname_target = nickname.downgrade();
        let password_target = password.downgrade();
        let dismiss_target = dismiss.downgrade();
        let invitation_for_join = invitation.clone();
        join.connect_clicked(move |_| {
            let (
                Some(ui),
                Some(dialog),
                Some(source),
                Some(feedback),
                Some(busy),
                Some(join),
                Some(nickname),
                Some(password),
                Some(dismiss),
            ) = (
                weak.upgrade(),
                dialog_target.upgrade(),
                source_target.upgrade(),
                feedback_target.upgrade(),
                busy_target.upgrade(),
                join_target.upgrade(),
                nickname_target.upgrade(),
                password_target.upgrade(),
                dismiss_target.upgrade(),
            )
            else {
                return;
            };
            let nick = nickname.text().trim().to_owned();
            if nick.is_empty() {
                status(&feedback, "Enter a room nickname.", true);
                return;
            }
            join.set_sensitive(false);
            dismiss.set_sensitive(false);
            nickname.set_sensitive(false);
            password.set_sensitive(false);
            busy.set_visible(true);
            status(&feedback, "Checking the room and joining…", false);
            let entered_password = Password(Zeroizing::new(password.text().to_string()));
            let weak = Rc::downgrade(&ui);
            let dialog = dialog.downgrade();
            let source = source.downgrade();
            let feedback = feedback.downgrade();
            let busy = busy.downgrade();
            let join = join.downgrade();
            let nickname = nickname.downgrade();
            let password = password.downgrade();
            let dismiss = dismiss.downgrade();
            let invitation = invitation_for_join.clone();
            let operation = ui.accept_room_invitation(&invitation, nick, Some(entered_password));
            drop(ui);
            glib::MainContext::default().spawn_local(async move {
                let result = operation.await;
                let (
                    Some(ui),
                    Some(dialog),
                    Some(source),
                    Some(feedback),
                    Some(busy),
                    Some(join),
                    Some(nickname),
                    Some(password),
                    Some(dismiss),
                ) = (
                    weak.upgrade(),
                    dialog.upgrade(),
                    source.upgrade(),
                    feedback.upgrade(),
                    busy.upgrade(),
                    join.upgrade(),
                    nickname.upgrade(),
                    password.upgrade(),
                    dismiss.upgrade(),
                )
                else {
                    return;
                };
                busy.set_visible(false);
                match result {
                    Ok(()) => {
                        source.set_visible(false);
                        ui.refresh_request_badge();
                        dialog.close();
                    }
                    Err(error) => {
                        status(&feedback, &error, true);
                        join.set_sensitive(true);
                        dismiss.set_sensitive(true);
                        nickname.set_sensitive(true);
                        password.set_sensitive(true);
                    }
                }
            });
        });

        let weak = Rc::downgrade(self);
        let dialog_target = dialog.downgrade();
        let source_target = source.downgrade();
        let feedback_target = feedback.downgrade();
        dismiss.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let confirm = adw::AlertDialog::builder()
                .heading("Dismiss invitation?")
                .body(format!(
                    "Remove the saved invitation to {}? A new invitation can appear later.",
                    invitation.room
                ))
                .build();
            confirm.add_responses(&[("cancel", "Cancel"), ("dismiss", "Dismiss")]);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");
            confirm.set_response_appearance("dismiss", adw::ResponseAppearance::Destructive);
            let weak = Rc::downgrade(&ui);
            let invitation = invitation.clone();
            let dialog = dialog_target.clone();
            let source = source_target.clone();
            let feedback = feedback_target.clone();
            confirm.connect_response(Some("dismiss"), move |_, _| {
                let (Some(ui), Some(dialog), Some(source), Some(feedback)) = (
                    weak.upgrade(),
                    dialog.upgrade(),
                    source.upgrade(),
                    feedback.upgrade(),
                ) else {
                    return;
                };
                match ui.dismiss_room_invitation(&invitation) {
                    Ok(true) => {
                        source.set_visible(false);
                        ui.refresh_request_badge();
                        dialog.close();
                    }
                    Ok(false) => status(
                        &feedback,
                        "This invitation changed. Reopen it to review the latest version.",
                        true,
                    ),
                    Err(error) => status(&feedback, &error, true),
                }
            });
            confirm.present(Some(&ui.window));
        });
    }
}
