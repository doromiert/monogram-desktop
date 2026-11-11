//! Native presentation for independent XMPP presence-subscription directions.
use crate::{
    contact_access::{Action, State},
    protocol::Command,
    ui::Ui,
    widgets::*,
};
use adw::prelude::*;
use gtk::glib;
use std::rc::Rc;

pub struct ContactAccessControls {
    generation: u64,
    owner: String,
    peer: String,
    content: glib::WeakRef<adw::Bin>,
    dialog: glib::WeakRef<adw::Dialog>,
    state: Option<State>,
    querying: bool,
    refresh_pending: bool,
}

fn cache_rendered_state(cache: &mut Option<State>, received: &State, will_render: bool) -> bool {
    let changed = cache.as_ref() != Some(received);
    if will_render {
        *cache = Some(received.clone());
    }
    changed
}

fn state_row(title: &str, value: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .build()
}

fn action_button(title: &str, appearance: &str) -> adw::ButtonRow {
    let button = adw::ButtonRow::builder()
        .title(title)
        .use_markup(false)
        .build();
    if !appearance.is_empty() {
        button.add_css_class(appearance);
    }
    button
}

impl Ui {
    pub fn contact_access(self: &Rc<Self>, peer: &str) {
        let content = adw::Bin::new();
        content.set_child(Some(
            &adw::StatusPage::builder()
                .title("Loading Contact Access")
                .description(peer)
                .child(&gtk::Spinner::builder().spinning(true).build())
                .build(),
        ));
        let (dialog, _) = self.dialog("Control Access", &content, 520, 600);
        self.contact_access_widgets
            .replace(Some(ContactAccessControls {
                generation: self.generation.get(),
                owner: self.state.borrow().account.clone(),
                peer: peer.to_owned(),
                content: content.downgrade(),
                dialog: dialog.downgrade(),
                state: None,
                querying: false,
                refresh_pending: false,
            }));
        let weak = Rc::downgrade(self);
        let closing = dialog.downgrade();
        dialog.connect_closed(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let mut active = ui.contact_access_widgets.borrow_mut();
            if active
                .as_ref()
                .is_some_and(|controls| controls.dialog.upgrade() == closing.upgrade())
            {
                active.take();
            }
        });
        self.refresh_contact_access();
    }

    pub(crate) fn refresh_contact_access(self: &Rc<Self>) {
        let Some((peer, owner, generation, content, dialog)) = ({
            let mut active = self.contact_access_widgets.borrow_mut();
            let Some(controls) = active.as_mut() else {
                return;
            };
            let Some(content) = controls.content.upgrade() else {
                active.take();
                return;
            };
            let Some(dialog) = controls.dialog.upgrade() else {
                active.take();
                return;
            };
            if controls.querying {
                controls.refresh_pending = true;
                return;
            }
            if controls.generation != self.generation.get()
                || controls.owner != self.state.borrow().account
                || !dialog.is_visible()
            {
                return;
            }
            controls.querying = true;
            Some((
                controls.peer.clone(),
                controls.owner.clone(),
                controls.generation,
                content,
                dialog,
            ))
        }) else {
            return;
        };
        let (reply, receive) = tokio::sync::oneshot::channel();
        if !self.command(Command::ContactAccessState {
            peer: peer.clone(),
            reply,
        }) {
            if let Some(controls) = self.contact_access_widgets.borrow_mut().as_mut() {
                controls.querying = false;
                controls.state = None;
            }
            self.render_contact_access_error(
                &peer,
                "Reconnect to view or change contact access.",
                &content,
                &dialog,
            );
            return;
        }
        let weak = Rc::downgrade(self);
        let content = content.downgrade();
        let dialog = dialog.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let result = receive.await;
            let (Some(ui), Some(content), Some(dialog)) =
                (weak.upgrade(), content.upgrade(), dialog.upgrade())
            else {
                return;
            };
            if ui.generation.get() != generation || ui.state.borrow().account != owner {
                return;
            }
            let (changed, refresh_pending) = {
                let mut active = ui.contact_access_widgets.borrow_mut();
                let Some(controls) = active.as_mut().filter(|controls| {
                    controls.generation == generation
                        && controls.owner == owner
                        && controls.peer == peer
                }) else {
                    return;
                };
                controls.querying = false;
                let refresh_pending = std::mem::take(&mut controls.refresh_pending);
                let changed = match &result {
                    Ok(Ok(state)) => {
                        cache_rendered_state(&mut controls.state, state, !refresh_pending)
                    }
                    _ => true,
                };
                if !refresh_pending && !matches!(&result, Ok(Ok(_))) {
                    controls.state = None;
                }
                (changed, refresh_pending)
            };
            if refresh_pending {
                ui.refresh_contact_access();
                return;
            }
            if !changed {
                return;
            }
            match result {
                Ok(Ok(state)) => ui.render_contact_access(&peer, state, None, &content, &dialog),
                Ok(Err(error)) => ui.render_contact_access_error(&peer, &error, &content, &dialog),
                Err(_) => ui.render_contact_access_error(
                    &peer,
                    "Contact access loading was interrupted.",
                    &content,
                    &dialog,
                ),
            }
        });
    }

    fn render_contact_access_error(
        self: &Rc<Self>,
        peer: &str,
        message: &str,
        content: &adw::Bin,
        dialog: &adw::Dialog,
    ) {
        let page = adw::StatusPage::builder()
            .title("Contact Access Unavailable")
            .description(message)
            .icon_name("network-offline-symbolic")
            .build();
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("suggested-action");
        retry.set_halign(gtk::Align::Center);
        page.set_child(Some(&retry));
        let weak = Rc::downgrade(self);
        let content_weak = content.downgrade();
        let dialog_weak = dialog.downgrade();
        let peer = peer.to_owned();
        retry.connect_clicked(move |_| {
            let (Some(ui), Some(content), Some(dialog)) = (
                weak.upgrade(),
                content_weak.upgrade(),
                dialog_weak.upgrade(),
            ) else {
                return;
            };
            content.set_child(Some(
                &adw::StatusPage::builder()
                    .title("Loading Contact Access")
                    .description(&peer)
                    .child(&gtk::Spinner::builder().spinning(true).build())
                    .build(),
            ));
            if let Some(controls) = ui.contact_access_widgets.borrow_mut().as_mut() {
                controls.state = None;
            }
            drop((content, dialog));
            ui.refresh_contact_access();
        });
        content.set_child(Some(&page));
    }

    fn render_contact_access(
        self: &Rc<Self>,
        peer: &str,
        state: State,
        notice: Option<&str>,
        content: &adw::Bin,
        dialog: &adw::Dialog,
    ) {
        let page = adw::PreferencesPage::new();
        let summary = adw::PreferencesGroup::builder()
            .title(peer)
            .description("Control access to your online status and profile data shared with contacts. Message encryption and fingerprint trust are managed separately.")
            .build();
        page.add(&summary);

        let incoming = adw::PreferencesGroup::builder()
            .title("Their Access to You")
            .build();
        let own_status = if !state.loaded {
            "Unavailable"
        } else if state.shares_own_presence {
            "Allowed"
        } else if state.pre_approved {
            "Pre-approved"
        } else if state.allow_requests {
            "Waiting for request"
        } else {
            "Not allowed"
        };
        incoming.add(&state_row("Your access list", own_status));
        if state.loaded {
            if state.shares_own_presence || state.pre_approved || state.allow_requests {
                let revoke = action_button("Revoke Access", "destructive-action");
                let weak = Rc::downgrade(self);
                let peer = peer.to_owned();
                let content = content.downgrade();
                let dialog = dialog.downgrade();
                revoke.connect_activated(move |_| {
                    let (Some(ui), Some(content), Some(dialog)) =
                        (weak.upgrade(), content.upgrade(), dialog.upgrade())
                    else {
                        return;
                    };
                    ui.confirm_revoke_contact_access(&peer, content, dialog);
                });
                incoming.add(&revoke);
            } else {
                self.add_contact_action(
                    &incoming,
                    "Allow Access",
                    "suggested-action",
                    peer,
                    Action::Allow,
                    content,
                    dialog,
                );
            }
            if state.incoming_pending
                && !state.shares_own_presence
                && (state.pre_approved || state.allow_requests)
            {
                self.add_contact_action(
                    &incoming,
                    "Allow This Request",
                    "suggested-action",
                    peer,
                    Action::Allow,
                    content,
                    dialog,
                );
            }
        }
        if state.allow_requests {
            incoming.set_description(Some("Monogram will approve this contact’s requests while signed in. Once approved, your server stores their access. Revoke access removes this permission and the server subscription."));
        } else if state.pre_approved && !state.shares_own_presence {
            incoming.set_description(Some("Your server reports this contact as pre-approved. Access becomes active when their client requests it."));
        }
        page.add(&incoming);

        let outgoing = adw::PreferencesGroup::builder()
            .title("Your Access to Them")
            .build();
        let their_status = if !state.loaded {
            "Unavailable"
        } else if state.sees_their_presence {
            "Shared with you"
        } else if state.outgoing_pending {
            "Request pending"
        } else {
            "Not shared"
        };
        outgoing.add(&state_row("Their presence", their_status));
        if state.loaded && state.outgoing_pending {
            self.add_contact_action(
                &outgoing,
                "Cancel Contact Request",
                "",
                peer,
                Action::Cancel,
                content,
                dialog,
            );
        } else if state.loaded && !state.sees_their_presence {
            self.add_contact_action(
                &outgoing,
                "Contact Request",
                "suggested-action",
                peer,
                Action::Request,
                content,
                dialog,
            );
        }
        page.add(&outgoing);

        if state.incoming_pending {
            let request = adw::PreferencesGroup::builder()
                .title("Incoming Request")
                .description("Allow access approves only their request. Accept and request back also asks them to share their presence with you.")
                .build();
            self.add_contact_action(
                &request,
                "Accept and Request Back",
                "suggested-action",
                peer,
                Action::Accept,
                content,
                dialog,
            );
            self.add_contact_action(
                &request,
                "Decline Request",
                "",
                peer,
                Action::Deny,
                content,
                dialog,
            );
            page.add(&request);
        }

        if !state.loaded {
            let connection = adw::PreferencesGroup::builder()
                .title("XMPP Connection")
                .build();
            connection.add(&state_row(
                "Access unavailable",
                "Reconnect to load or change contact access",
            ));
            page.add(&connection);
        }

        if let Some(notice) = notice {
            let feedback = adw::PreferencesGroup::new();
            feedback.add(&wrap(notice, "dim-label"));
            page.add(&feedback);
        }
        content.set_child(Some(&scroll(&page)));
    }

    fn add_contact_action(
        self: &Rc<Self>,
        group: &adw::PreferencesGroup,
        title: &str,
        appearance: &str,
        peer: &str,
        action: Action,
        content: &adw::Bin,
        dialog: &adw::Dialog,
    ) {
        let button = action_button(title, appearance);
        let weak = Rc::downgrade(self);
        let peer = peer.to_owned();
        let content = content.downgrade();
        let dialog = dialog.downgrade();
        button.connect_activated(move |_| {
            let (Some(ui), Some(content), Some(dialog)) =
                (weak.upgrade(), content.upgrade(), dialog.upgrade())
            else {
                return;
            };
            ui.run_contact_access_action(&peer, action, content, dialog);
        });
        group.add(&button);
    }

    fn confirm_revoke_contact_access(
        self: &Rc<Self>,
        peer: &str,
        content: adw::Bin,
        dialog: adw::Dialog,
    ) {
        let confirm = adw::AlertDialog::builder()
            .heading("Revoke Access?")
            .body("Remove this contact’s access to your online status and contact-only profile data, and stop automatically approving their requests?")
            .build();
        confirm.add_responses(&[("cancel", "Cancel"), ("revoke", "Revoke Access")]);
        confirm.set_default_response(Some("cancel"));
        confirm.set_close_response("cancel");
        confirm.set_response_appearance("revoke", adw::ResponseAppearance::Destructive);
        let weak = Rc::downgrade(self);
        let peer = peer.to_owned();
        confirm.connect_response(Some("revoke"), move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.run_contact_access_action(
                    &peer,
                    Action::Revoke,
                    content.clone(),
                    dialog.clone(),
                );
            }
        });
        confirm.present(Some(&self.window));
    }

    fn run_contact_access_action(
        self: &Rc<Self>,
        peer: &str,
        action: Action,
        content: adw::Bin,
        dialog: adw::Dialog,
    ) {
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let active = self
            .contact_access_widgets
            .borrow_mut()
            .as_mut()
            .is_some_and(|controls| {
                if controls.generation == generation
                    && controls.owner == owner
                    && controls.peer == peer
                    && !controls.querying
                {
                    controls.querying = true;
                    true
                } else {
                    false
                }
            });
        if !active {
            return;
        }
        content.set_child(Some(
            &adw::StatusPage::builder()
                .title("Updating Contact Access")
                .description(peer)
                .child(&gtk::Spinner::builder().spinning(true).build())
                .build(),
        ));
        let (reply, receive) = tokio::sync::oneshot::channel();
        if !self.command(Command::ContactAccess {
            peer: peer.to_owned(),
            action,
            reply,
        }) {
            if let Some(controls) = self.contact_access_widgets.borrow_mut().as_mut() {
                controls.querying = false;
                controls.state = None;
            }
            self.render_contact_access_error(
                peer,
                "Reconnect to change contact access.",
                &content,
                &dialog,
            );
            return;
        }
        let weak = Rc::downgrade(self);
        let peer = peer.to_owned();
        let content = content.downgrade();
        let dialog = dialog.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let result = receive.await;
            let (Some(ui), Some(content), Some(dialog)) =
                (weak.upgrade(), content.upgrade(), dialog.upgrade())
            else {
                return;
            };
            if ui.generation.get() != generation || ui.state.borrow().account != owner {
                return;
            }
            let refresh_pending = {
                let mut active = ui.contact_access_widgets.borrow_mut();
                let Some(controls) = active.as_mut().filter(|controls| {
                    controls.generation == generation
                        && controls.owner == owner
                        && controls.peer == peer
                }) else {
                    return;
                };
                controls.querying = false;
                controls.state = match &result {
                    Ok(Ok(state)) => Some(state.clone()),
                    _ => None,
                };
                std::mem::take(&mut controls.refresh_pending)
            };
            match result {
                Ok(Ok(state)) => ui.render_contact_access(
                    &peer,
                    state,
                    Some(match action {
                        Action::Allow => "Automatic approval is saved on this device. Confirmed server access is shown separately.",
                        Action::Request => "Request sent. Their sharing status changes when the contact or server confirms it.",
                        Action::Accept => "Response sent. Each direction remains shown separately.",
                        Action::Deny => "Request declined. The latest server state is shown above.",
                        Action::Cancel => "Cancellation sent. The latest server state is shown above.",
                        Action::Revoke => "Revocation sent. The latest server state is shown above.",
                    }),
                    &content,
                    &dialog,
                ),
                Ok(Err(error)) => {
                    ui.render_contact_access_error(&peer, &error, &content, &dialog)
                }
                Err(_) => ui.render_contact_access_error(
                    &peer,
                    "The contact access update was interrupted.",
                    &content,
                    &dialog,
                ),
            }
            if refresh_pending {
                ui.refresh_contact_access();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_reply_does_not_advance_rendered_cache() {
        let received = State {
            loaded: true,
            sees_their_presence: true,
            ..Default::default()
        };
        let mut rendered = None;

        assert!(cache_rendered_state(&mut rendered, &received, false));
        assert_eq!(rendered, None);
        assert!(cache_rendered_state(&mut rendered, &received, true));
        assert_eq!(rendered, Some(received.clone()));
        assert!(!cache_rendered_state(&mut rendered, &received, true));
    }
}
