use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use zeroize::Zeroizing;

pub fn attach(
    page: &adw::PreferencesPage,
    dialog: &adw::Dialog,
    jid: &adw::EntryRow,
    password: &adw::PasswordEntryRow,
) {
    let group=adw::PreferencesGroup::builder().title("KeePassXC")
        .description("Open KeePassXC and enable browser integration. Enter your XMPP address, then approve Monogram’s connection in KeePassXC.").build();
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let fill = gtk::Button::with_label("Fill from KeePassXC");
    let save = gtk::Button::with_label("Save to KeePassXC");
    let forget = gtk::Button::from_icon_name("edit-clear-symbolic");
    forget.set_tooltip_text(Some("Disconnect KeePassXC for this account"));
    let spinner = gtk::Spinner::new();
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    for child in [&fill, &save, &forget] {
        buttons.append(child);
    }
    buttons.append(&spinner);
    body.append(&buttons);
    body.append(&status);
    group.add(&body);
    page.add(&group);
    let active = Rc::new(RefCell::new(gio::Cancellable::new()));
    let generation = Rc::new(Cell::new(0u64));
    let busy = Rc::new(Cell::new(false));
    for field in [
        jid.upcast_ref::<gtk::Editable>(),
        password.upcast_ref::<gtk::Editable>(),
    ] {
        let active = active.clone();
        let generation = generation.clone();
        let busy = busy.clone();
        let spinner = spinner.clone();
        let status = status.clone();
        let buttons = buttons.clone();
        field.connect_changed(move |_| {
            active.borrow().cancel();
            generation.set(generation.get() + 1);
            busy.set(false);
            spinner.stop();
            buttons.set_sensitive(true);
            status.set_text("");
        });
    }
    let a = active.clone();
    let g = generation.clone();
    dialog.connect_closed(move |_| {
        a.borrow().cancel();
        g.set(g.get() + 1);
    });
    for (button, kind) in [(&fill, 0), (&save, 1), (&forget, 2)] {
        let jid = jid.downgrade();
        let password = password.downgrade();
        let active = active.clone();
        let generation = generation.clone();
        let busy = busy.clone();
        let status = status.clone();
        let spinner = spinner.clone();
        let buttons = buttons.clone();
        button.connect_clicked(move |_| {
            if busy.get(){return;}
            let (Some(jid),Some(password))=(jid.upgrade(),password.upgrade()) else{return;};
            let address=match crate::protocol::bare(jid.text().as_str()) {Ok(address) if address.contains('@')=>address,_=>{status.set_text("Enter your full XMPP address first.");return;}};
            if kind==1 && password.text().is_empty(){status.set_text("Enter a password to save first.");return;}
            let secret=if kind==1 {Some(Zeroizing::new(password.text().to_string()))}else{None};
            active.borrow().cancel();let cancel=gio::Cancellable::new();active.replace(cancel.clone());
            generation.set(generation.get()+1);let current=generation.get();busy.set(true);buttons.set_sensitive(false);spinner.start();
            status.set_text(if kind==2 {"Disconnecting…"}else{"Waiting for KeePassXC. Unlock the database and approve its prompt."});
            let password=password.downgrade();let generation=generation.clone();let busy=busy.clone();let status=status.clone();let spinner=spinner.clone();let buttons=buttons.clone();
            glib::MainContext::default().spawn_local(async move {
                let result=if kind==2 {crate::keepassxc::disconnect(&address).await.map(|_|None)}else{
                    crate::keepassxc::run(address,secret.map(crate::keepassxc::Action::Save).unwrap_or(crate::keepassxc::Action::Fill),cancel).await
                };
                if generation.get()!=current{return;}
                busy.set(false);spinner.stop();buttons.set_sensitive(true);
                match result {
                    Ok(Some(login))=>{if let Some(password)=password.upgrade(){password.set_text(&login.password);}status.set_text("Password filled. Press Connect to sign in.");},
                    Ok(None)=>status.set_text(if kind==2 {"Disconnected. Revoke Monogram’s key in KeePassXC to remove database authorization too."}else{"Save request accepted by KeePassXC."}),
                    Err(error)=>status.set_text(error),
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "private libsodium proxy, keyring and native display fixture"]
    fn native_fill_feedback() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        adw::init().unwrap();
        let context = glib::MainContext::default();
        context.block_on(async {
            let parent = gtk::Window::new();
            parent.set_default_size(520, 520);
            parent.present();
            let dialog = adw::Dialog::new();
            let page = adw::PreferencesPage::new();
            let jid = adw::EntryRow::builder()
                .text("alice@example.invalid")
                .build();
            let password = adw::PasswordEntryRow::new();
            let fields = adw::PreferencesGroup::new();
            fields.add(&jid);
            fields.add(&password);
            page.add(&fields);
            attach(&page, &dialog, &jid, &password);
            dialog.set_child(Some(&page));
            dialog.present(Some(&parent));
            fn find(widget: &gtk::Widget) -> Option<gtk::Button> {
                if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                    if button.label().as_deref() == Some("Fill from KeePassXC") {
                        return Some(button.clone());
                    }
                }
                let mut child = widget.first_child();
                while let Some(c) = child {
                    if let Some(button) = find(&c) {
                        return Some(button);
                    }
                    child = c.next_sibling();
                }
                None
            }
            let fill = find(page.upcast_ref()).unwrap();
            let begin = std::time::Instant::now();
            fill.emit_clicked();
            assert!(!fill.is_sensitive());
            assert!(begin.elapsed() < std::time::Duration::from_millis(400));
            let mut previous = begin;
            let mut largest_gap = std::time::Duration::ZERO;
            while password.text().is_empty() && begin.elapsed() < std::time::Duration::from_secs(8)
            {
                glib::timeout_future(std::time::Duration::from_millis(10)).await;
                let now = std::time::Instant::now();
                largest_gap = largest_gap.max(now - previous);
                previous = now;
            }
            assert_eq!(password.text().as_str(), "fictional login");
            assert!(fill.is_sensitive());
            assert!(
                largest_gap < std::time::Duration::from_millis(400),
                "UI gap: {largest_gap:?}"
            );
            println!(
                "KeePassXC fill: {:?}, maximum UI gap: {largest_gap:?}",
                begin.elapsed()
            );
            dialog.force_close();
            password.set_text("");
            parent.destroy();
        });
    }
}
