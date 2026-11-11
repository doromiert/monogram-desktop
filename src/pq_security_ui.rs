use crate::{encryption_policy::Mode, protocol, ui::Ui, widgets::*};
use adw::prelude::*;
use std::{collections::BTreeMap, rc::Rc};

fn grouped(fingerprint: &str) -> String {
    fingerprint
        .as_bytes()
        .chunks(8)
        .enumerate()
        .map(|(index, chunk)| {
            let value = std::str::from_utf8(chunk).unwrap_or("");
            if index > 0 && index % 4 == 0 {
                format!("\n{value}")
            } else if index > 0 {
                format!(" {value}")
            } else {
                value.into()
            }
        })
        .collect()
}

impl Ui {
    pub fn handle_pq_review(self: &Rc<Self>, peer: &str) {
        if !self
            .review_request
            .borrow()
            .as_ref()
            .is_some_and(|(target, mode)| target == peer && *mode == Mode::Pq)
        {
            return;
        }
        self.review_request.borrow_mut().take();
        let rows = self
            .pq_reviews
            .borrow_mut()
            .remove(peer)
            .unwrap_or_default();
        let owner = self.state.borrow().account.clone();
        let page = adw::PreferencesPage::new();
        let contact = adw::PreferencesGroup::builder()
            .title("Contact Devices")
            .description("Compare the complete hybrid fingerprint through another trusted channel")
            .build();
        let own = adw::PreferencesGroup::builder()
            .title("Other Devices on Your Account")
            .description("Blind trust for this conversation approves your own devices too; otherwise review each hybrid fingerprint independently. Explicit blocks remain blocked.")
            .build();
        let mut keys: BTreeMap<String, BTreeMap<u32, String>> = BTreeMap::new();
        let mut own_count = 0;
        let mut contact_count = 0;
        for row in rows {
            if row.fingerprint.len() != 128
                || !row.fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                continue;
            }
            keys.entry(row.peer.clone())
                .or_default()
                .insert(row.device, row.fingerprint.clone());
            let device = adw::ExpanderRow::builder()
                .title(if row.peer == owner {
                    format!("Your Device {}", row.device)
                } else {
                    format!("Device {}", row.device)
                })
                .subtitle(format!("{} · {}", row.peer, row.trust))
                .use_markup(false)
                .expanded(true)
                .build();
            let fingerprint = wrap(&grouped(&row.fingerprint), "monospace");
            fingerprint.set_selectable(true);
            fingerprint.update_property(&[gtk::accessible::Property::Label(&row.fingerprint)]);
            margins(&fingerprint, 12);
            device.add_row(&fingerprint);
            if row.peer == owner {
                own.add(&device);
                own_count += 1;
            } else {
                contact.add(&device);
                contact_count += 1;
            }
        }
        if contact_count == 0 {
            contact.add(
                &adw::ActionRow::builder()
                    .title("No Contact Devices Found")
                    .build(),
            );
        }
        page.add(&contact);
        if own_count > 0 {
            page.add(&own);
        }
        let (dialog, header) = self.dialog("OMEMO2 Hybrid Fingerprints", &page, 640, 640);
        let generation = self.generation.get();
        for (approve, title, style) in [
            (false, "Block Shown", "destructive-action"),
            (true, "Trust Shown", "suggested-action"),
        ] {
            let button = gtk::Button::with_label(title);
            button.add_css_class(style);
            button.set_sensitive(!keys.is_empty());
            let weak = Rc::downgrade(self);
            let dialog = dialog.downgrade();
            let keys = keys.clone();
            let target = peer.to_string();
            button.connect_clicked(move |button| {
                let Some(ui) = weak
                    .upgrade()
                    .filter(|ui| ui.generation.get() == generation)
                else {
                    return;
                };
                button.set_sensitive(false);
                ui.command(protocol::Command::PqCrypto(
                    crate::crypto::pq::service::Request::Decide {
                        keys: keys.clone(),
                        approve,
                    },
                ));
                ui.invalidate_sending_choice(&target);
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
            });
            if approve {
                header.pack_end(&button);
            } else {
                header.pack_start(&button);
            }
        }
    }
}
