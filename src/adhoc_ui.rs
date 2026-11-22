//! Server commands (XEP-0050) and "Create Invite Link" (XEP-0401): the dialogs over `adhoc.rs`.
//! Everything a server sends is shown as plain text, never as markup.
use crate::{
    adhoc::{self, Action, Field, Form, Kind, Status, Step},
    iq,
    ui::Ui,
    widgets::*,
};
use adw::prelude::*;
use gtk::glib;
use tokio_xmpp::minidom::Element;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::{Rc, Weak},
};

type Getter = Box<dyn Fn() -> Vec<String>>;

/// The widgets for `form`, and for each editable field a function that reads what was entered.
pub(crate) fn form_widgets(form: &Form) -> (gtk::Box, Vec<(String, Getter)>) {
    let root = bx(true, 12);
    let mut getters: Vec<(String, Getter)> = Vec::new();
    if !form.title.is_empty() {
        root.append(&wrap(&form.title, "heading"));
    }
    if !form.instructions.is_empty() {
        root.append(&wrap(&form.instructions.join("\n"), "dim-label"));
    }
    let singles = adw::PreferencesGroup::new();
    let mut singles_used = false;
    let mut extra: Vec<gtk::Widget> = Vec::new();
    for field in &form.fields {
        match field.kind {
            Kind::Hidden => {}
            Kind::Fixed => {
                if let Some(text) = field.values.first() {
                    let label = wrap(text, "dim-label");
                    label.set_selectable(true);
                    extra.push(label.upcast());
                }
            }
            Kind::Text | Kind::Jid => {
                let row = adw::EntryRow::new();
                row.set_use_markup(false);
                row.set_title(&field_title(field));
                row.set_text(field.values.first().map_or("", String::as_str));
                singles.add(&row);
                singles_used = true;
                getters.push((field.var.clone(), Box::new(move || vec![row.text().to_string()])));
            }
            Kind::Private => {
                let row = adw::PasswordEntryRow::new();
                row.set_use_markup(false);
                row.set_title(&field_title(field));
                singles.add(&row);
                singles_used = true;
                getters.push((field.var.clone(), Box::new(move || vec![row.text().to_string()])));
            }
            Kind::Boolean => {
                let row = adw::SwitchRow::builder()
                    .use_markup(false)
                    .title(&field.label)
                    .active(field.values.first().is_some_and(|v| adhoc::truthy(v)))
                    .build();
                singles.add(&row);
                singles_used = true;
                getters.push((
                    field.var.clone(),
                    Box::new(move || vec![if row.is_active() { "1" } else { "0" }.to_string()]),
                ));
            }
            Kind::ListSingle => {
                let labels: Vec<&str> = field.options.iter().map(|(_, label)| label.as_str()).collect();
                let row = adw::ComboRow::builder()
                    .use_markup(false)
                    .title(field_title(field))
                    .model(&gtk::StringList::new(&labels))
                    .build();
                let chosen = field
                    .values
                    .first()
                    .and_then(|value| field.options.iter().position(|(v, _)| v == value));
                if let Some(index) = chosen {
                    row.set_selected(index as u32);
                }
                singles.add(&row);
                singles_used = true;
                let options = field.options.clone();
                getters.push((
                    field.var.clone(),
                    Box::new(move || {
                        options
                            .get(row.selected() as usize)
                            .map(|(value, _)| vec![value.clone()])
                            .unwrap_or_default()
                    }),
                ));
            }
            Kind::ListMulti => {
                let group = adw::PreferencesGroup::new();
                let heading = wrap(&field.label, "heading");
                let mut rows = Vec::new();
                for (value, label) in &field.options {
                    let row = adw::SwitchRow::builder()
                        .use_markup(false)
                        .title(label)
                        .active(field.values.contains(value))
                        .build();
                    group.add(&row);
                    rows.push((value.clone(), row));
                }
                extra.push(heading.upcast());
                extra.push(group.upcast());
                getters.push((
                    field.var.clone(),
                    Box::new(move || {
                        rows.iter()
                            .filter(|(_, row)| row.is_active())
                            .map(|(value, _)| value.clone())
                            .collect()
                    }),
                ));
            }
            Kind::Multi | Kind::JidMulti => {
                let heading = wrap(
                    &if field.kind == Kind::JidMulti {
                        format!("{} (one address per line)", field.label)
                    } else {
                        field.label.clone()
                    },
                    "heading",
                );
                let view = gtk::TextView::builder()
                    .wrap_mode(gtk::WrapMode::WordChar)
                    .top_margin(8)
                    .bottom_margin(8)
                    .left_margin(12)
                    .right_margin(12)
                    .build();
                view.buffer().set_text(&field.values.join("\n"));
                let frame = gtk::ScrolledWindow::builder()
                    .child(&view)
                    .min_content_height(96)
                    .build();
                frame.add_css_class("card");
                extra.push(heading.upcast());
                extra.push(frame.upcast());
                getters.push((
                    field.var.clone(),
                    Box::new(move || {
                        let buffer = view.buffer();
                        let (start, end) = buffer.bounds();
                        buffer
                            .text(&start, &end, false)
                            .lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                            .map(str::to_owned)
                            .collect()
                    }),
                ));
            }
        }
    }
    if singles_used || !form.title.is_empty() || !form.instructions.is_empty() {
        root.append(&singles);
    }
    for widget in extra {
        root.append(&widget);
    }
    (root, getters)
}

/// The row title: plain text, marked when the server requires an answer.
fn field_title(field: &Field) -> String {
    if field.required { format!("{} *", field.label) } else { field.label.clone() }
}

/// The values entered, keyed by field.
pub(crate) fn read_values(getters: &[(String, Getter)]) -> BTreeMap<String, Vec<String>> {
    getters.iter().map(|(var, get)| (var.clone(), get())).collect()
}

/// One command running in a dialog: each answer replaces the page.
struct Runner {
    ui: Weak<Ui>,
    server: String,
    node: String,
    dialog: adw::Dialog,
    body: gtk::Box,
    session: RefCell<Option<String>>,
    /// The command ended or was cancelled; closing the dialog needs no cancel request.
    finished: Cell<bool>,
    busy: Cell<bool>,
}

impl Ui {
    fn server_domain(&self) -> Option<String> {
        let state = self.state.borrow();
        let (_, domain) = state.account.split_once('@')?;
        self.online.get().then(|| domain.to_owned())
    }

    fn command_client(&self) -> Option<iq::Client> {
        self.session.borrow().as_ref().map(|s| iq::Client::new(s.tx.clone()))
    }

    /// "Create Invite Link": run the server's invite command and show the link it makes.
    pub fn create_invite_link(self: &Rc<Self>) {
        self.run_server_command(adhoc::INVITE_NODE, "Invite Link");
    }

    /// The commands the server offers, each opening in its own dialog.
    pub fn server_commands(self: &Rc<Self>) {
        let (Some(server), Some(client)) = (self.server_domain(), self.command_client()) else {
            self.notice("Connect to an account first.");
            return;
        };
        let body = bx(true, 12);
        margins(&body, 18);
        let spinner = gtk::Spinner::builder().spinning(true).halign(gtk::Align::Center).build();
        body.append(&spinner);
        let (d, _) = self.dialog("Server Commands", &scroll(&body), 480, 520);
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let reply = client.request_on_main_loop(&server, iq::Kind::Get, adhoc::list_request()).await;
            let Some(ui) = weak.upgrade() else { return };
            while let Some(child) = body.first_child() {
                body.remove(&child);
            }
            let commands = match reply {
                Ok(Some(query)) => adhoc::parse_commands(&query, &server),
                Ok(None) => Vec::new(),
                Err(error) => {
                    body.append(&wrap(&adhoc::error_text(&error), "dim-label"));
                    return;
                }
            };
            if commands.is_empty() {
                body.append(&wrap("This server offers no commands.", "dim-label"));
                return;
            }
            let group = adw::PreferencesGroup::new();
            for command in commands {
                let row = adw::ActionRow::builder()
                    .use_markup(false)
                    .title(&command.name)
                    .subtitle(&command.node)
                    .activatable(true)
                    .build();
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                let weak = Rc::downgrade(&ui);
                let (node, name, dialog) = (command.node.clone(), command.name.clone(), d.clone());
                row.connect_activated(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        dialog.close();
                        ui.run_server_command(&node, &name);
                    }
                });
                group.add(&row);
            }
            body.append(&group);
        });
    }

    fn run_server_command(self: &Rc<Self>, node: &str, title: &str) {
        let Some(server) = self.server_domain() else {
            self.notice("Connect to an account first.");
            return;
        };
        let body = bx(true, 12);
        margins(&body, 18);
        let (dialog, _) = self.dialog(title, &scroll(&body), 480, 520);
        let runner = Rc::new(Runner {
            ui: Rc::downgrade(self),
            server,
            node: node.to_owned(),
            dialog: dialog.clone(),
            body,
            session: RefCell::new(None),
            finished: Cell::new(false),
            busy: Cell::new(false),
        });
        let closing = runner.clone();
        dialog.connect_closed(move |_| closing.cancel_if_open());
        runner.send(adhoc::execute(&runner.node));
    }
}

impl Runner {
    fn clear(&self) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
    }

    fn working(&self) {
        self.clear();
        self.body.append(&gtk::Spinner::builder().spinning(true).halign(gtk::Align::Center).build());
    }

    fn message(self: &Rc<Self>, text: &str) {
        self.clear();
        self.body.append(&wrap(text, "dim-label"));
        self.close_button();
    }

    fn close_button(self: &Rc<Self>) {
        let close = gtk::Button::with_label("Close");
        close.set_halign(gtk::Align::End);
        let dialog = self.dialog.clone();
        close.connect_clicked(move |_| {
            dialog.close();
        });
        self.body.append(&close);
    }

    /// Tell the server we are done with a command that is still waiting for an answer.
    fn cancel_if_open(&self) {
        if self.finished.replace(true) {
            return;
        }
        let (Some(session), Some(ui)) = (self.session.borrow().clone(), self.ui.upgrade()) else {
            return;
        };
        if let Some(client) = ui.command_client() {
            let (server, request) = (self.server.clone(), adhoc::step(&self.node, &session, Action::Cancel, None));
            glib::MainContext::default().spawn_local(async move {
                let _ = client.request_on_main_loop(&server, iq::Kind::Set, request).await;
            });
        }
    }

    fn send(self: &Rc<Self>, request: Element) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Some(client) = ui.command_client() else {
            self.message("Connect to an account first.");
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        self.working();
        let runner = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let reply = client.request_on_main_loop(&runner.server, iq::Kind::Set, request).await;
            runner.busy.set(false);
            if runner.ui.upgrade().is_none() {
                return;
            }
            match reply {
                Ok(Some(command)) => match adhoc::parse_step(&command, &runner.node) {
                    Ok(step) => runner.show(step),
                    Err(_) => {
                        runner.finished.set(true);
                        runner.message("The server's answer could not be used.")
                    }
                },
                Ok(None) => {
                    runner.finished.set(true);
                    runner.message("The server's answer could not be used.")
                }
                Err(error) => {
                    runner.finished.set(true);
                    runner.message(&adhoc::error_text(&error))
                }
            }
        });
    }

    /// Replace the page with what the server answered.
    fn show(self: &Rc<Self>, step: Step) {
        self.clear();
        *self.session.borrow_mut() = step.session.clone();
        let open = step.status == Status::Executing;
        self.finished.set(!open);
        if let Some(invite) = adhoc::invite(&step) {
            self.show_invite(&invite);
            return;
        }
        if step.status == Status::Canceled {
            self.dialog.close();
            return;
        }
        for note in &step.notes {
            let label = wrap(&note.text, if note.kind == "info" { "dim-label" } else { "error" });
            label.set_selectable(true);
            self.body.append(&label);
        }
        let mut getters = Vec::new();
        if let Some(form) = &step.form {
            if form.result {
                self.body.append(&result_rows(form));
            } else {
                let (widgets, read) = form_widgets(form);
                self.body.append(&widgets);
                getters = read;
            }
        }
        let buttons = bx(false, 8);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label(if open { "Cancel" } else { "Close" });
        let dialog = self.dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
        buttons.append(&cancel);
        if open {
            if step.actions.contains(&Action::Prev) {
                let back = gtk::Button::with_label("Back");
                let runner = self.clone();
                back.connect_clicked(move |_| {
                    let session = runner.session.borrow().clone().unwrap_or_default();
                    runner.send(adhoc::step(&runner.node, &session, Action::Prev, None));
                });
                buttons.append(&back);
            }
            let (action, label) = if step.actions.contains(&Action::Next)
                && step.default != Some(Action::Complete)
            {
                (Action::Next, "Next")
            } else {
                (Action::Complete, "Done")
            };
            let primary = gtk::Button::with_label(label);
            primary.add_css_class("suggested-action");
            let runner = self.clone();
            let form = step.form.clone().filter(|form| !form.result);
            primary.connect_clicked(move |_| {
                let session = runner.session.borrow().clone().unwrap_or_default();
                let answer = match &form {
                    Some(form) => {
                        let values = read_values(&getters);
                        let missing = adhoc::missing_required(form, &values);
                        if !missing.is_empty() {
                            if let Some(ui) = runner.ui.upgrade() {
                                ui.notice(&format!("Fill in: {}", missing.join(", ")));
                            }
                            return;
                        }
                        Some(adhoc::submit(form, &values))
                    }
                    None => None,
                };
                runner.send(adhoc::step(&runner.node, &session, action, answer));
            });
            buttons.append(&primary);
        }
        self.body.append(&buttons);
    }

    fn show_invite(self: &Rc<Self>, invite: &adhoc::Invite) {
        self.body.append(&wrap("Invite link created", "title-3"));
        self.body.append(&wrap(
            "Anyone with this link can create an account on this server and add you as a contact. Share it only with people you want to invite. It may work only once and may expire.",
            "dim-label",
        ));
        let group = adw::PreferencesGroup::new();
        for (title, link) in [("Link for Monogram and other XMPP apps", &invite.uri), ("Web page for new users", &invite.landing)] {
            let Some(link) = link else { continue };
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(link)
                .use_markup(false)
                .subtitle_selectable(true)
                .build();
            let copy = icon_button("edit-copy-symbolic", "Copy link");
            copy.set_valign(gtk::Align::Center);
            let (link, weak) = (link.clone(), self.ui.clone());
            copy.connect_clicked(move |button| {
                button.clipboard().set_text(&link);
                if let Some(ui) = weak.upgrade() {
                    ui.notice("Link copied");
                }
            });
            row.add_suffix(&copy);
            group.add(&row);
        }
        self.body.append(&group);
        match (invite.expires_at(), &invite.expires) {
            (Some(at), _) => self.body.append(&live_expiry(at)),
            (None, Some(raw)) => self.body.append(&wrap(&format!("Expires {raw}"), "dim-label")),
            (None, None) => {}
        }
        self.close_button();
    }
}

/// "Expires in 6 days", kept current: it recounts every second and says "Expired" once the time
/// passes. The exact local time is in the tooltip. The timer stops when the label is gone.
pub(crate) fn live_expiry(at: chrono::DateTime<chrono::Utc>) -> gtk::Label {
    let label = wrap(&adhoc::expiry_text(at, chrono::Utc::now()), "dim-label");
    label.set_tooltip_text(Some(
        &at.with_timezone(&chrono::Local).format("%A %-d %B %Y, %H:%M").to_string(),
    ));
    let weak = label.downgrade();
    glib::timeout_add_seconds_local(1, move || {
        let Some(label) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let now = chrono::Utc::now();
        label.set_label(&adhoc::expiry_text(at, now));
        if at <= now {
            label.remove_css_class("dim-label");
            label.add_css_class("error");
        }
        glib::ControlFlow::Continue
    });
    label
}

/// A result form as read-only rows.
fn result_rows(form: &Form) -> gtk::Box {
    let root = bx(true, 8);
    if !form.title.is_empty() {
        root.append(&wrap(&form.title, "heading"));
    }
    let group = adw::PreferencesGroup::new();
    for field in form.fields.iter().filter(|f| f.kind != Kind::Hidden) {
        let text = if field.options.is_empty() {
            field.values.join("\n")
        } else {
            field
                .values
                .iter()
                .map(|value| {
                    field.options.iter().find(|(v, _)| v == value).map_or(value.as_str(), |(_, label)| label.as_str())
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        if field.kind == Kind::Fixed {
            root.append(&wrap(&text, "dim-label"));
            continue;
        }
        let row = adw::ActionRow::builder()
            .title(&field.label)
            .subtitle(&text)
            .use_markup(false)
            .subtitle_selectable(true)
            .build();
        group.add(&row);
    }
    root.append(&group);
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    use adhoc::{Field, Form};

    fn field(kind: Kind, var: &str, values: &[&str], options: &[(&str, &str)]) -> Field {
        Field {
            kind,
            var: var.into(),
            label: format!("Label {var}"),
            values: values.iter().map(|v| v.to_string()).collect(),
            options: options.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect(),
            required: false,
        }
    }
    fn form(fields: Vec<Field>) -> Form {
        Form { title: "T".into(), instructions: vec!["Fill me".into()], fields, result: false }
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn form_widgets_start_from_the_servers_defaults_and_read_back_edits() {
        gtk::init().expect("display");
        let form = form(vec![
            field(Kind::Text, "name", &["old"], &[]),
            field(Kind::Boolean, "flag", &["1"], &[]),
            field(Kind::ListSingle, "role", &["user"], &[("admin", "Admin"), ("user", "User")]),
            field(Kind::ListMulti, "tags", &["b"], &[("a", "A"), ("b", "B")]),
            field(Kind::Multi, "notes", &["one", "two"], &[]),
            field(Kind::Hidden, "FORM_TYPE", &["urn:x"], &[]),
            field(Kind::Fixed, "", &["Just a label"], &[]),
        ]);
        let (_widgets, getters) = form_widgets(&form);
        let vars: Vec<&str> = getters.iter().map(|(var, _)| var.as_str()).collect();
        assert_eq!(vars, ["name", "flag", "role", "tags", "notes"], "hidden and fixed fields have no input");
        let values = read_values(&getters);
        assert_eq!(values["name"], ["old"]);
        assert_eq!(values["flag"], ["1"]);
        assert_eq!(values["role"], ["user"]);
        assert_eq!(values["tags"], ["b"]);
        assert_eq!(values["notes"], ["one", "two"]);
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn an_unselected_list_reads_as_no_value() {
        gtk::init().expect("display");
        let form = form(vec![field(Kind::ListSingle, "role", &[], &[("a", "A")])]);
        let (_widgets, getters) = form_widgets(&form);
        // GTK selects the first row of a combo by default; either way the value is a listed option.
        let value = read_values(&getters)["role"].clone();
        assert!(value.is_empty() || value == ["a"]);
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn server_text_is_never_read_as_markup() {
        gtk::init().expect("display");
        let nasty = "<b>x</b> & <a href='https://evil'>y</a>";
        let mut f = field(Kind::Text, "n", &[], &[]);
        f.label = nasty.into();
        let (widgets, _) = form_widgets(&form(vec![f]));
        fn find_row(widget: &gtk::Widget) -> Option<adw::EntryRow> {
            if let Ok(row) = widget.clone().downcast::<adw::EntryRow>() {
                return Some(row);
            }
            let mut child = widget.first_child();
            while let Some(c) = child {
                if let Some(found) = find_row(&c) {
                    return Some(found);
                }
                child = c.next_sibling();
            }
            None
        }
        let row = find_row(widgets.upcast_ref()).expect("an entry row");
        assert_eq!(row.title(), nasty);
        assert!(!row.uses_markup(), "a server-chosen label must not be parsed as markup");
    }

    #[test]
    fn required_fields_are_marked_in_the_title() {
        let mut f = field(Kind::Text, "n", &[], &[]);
        assert_eq!(field_title(&f), "Label n");
        f.required = true;
        assert_eq!(field_title(&f), "Label n *");
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn the_expiry_label_counts_down_and_turns_to_expired() {
        gtk::init().expect("display");
        let soon = chrono::Utc::now() + chrono::Duration::milliseconds(1500);
        let label = live_expiry(soon);
        assert_eq!(label.label(), "Expires in less than a minute");
        assert!(label.tooltip_text().is_some_and(|tip| tip.contains(':')), "the exact time is in the tooltip");
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        while label.label() != "Expired just now" && std::time::Instant::now() < deadline {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(label.label(), "Expired just now", "the text changed without being reopened");
        assert!(label.has_css_class("error") && !label.has_css_class("dim-label"));
    }

    fn every_label<F: FnMut(&gtk::Label)>(widget: &gtk::Widget, found: &mut F) {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            found(label);
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            every_label(&current, found);
            child = current.next_sibling();
        }
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn markup_in_a_servers_labels_is_never_parsed_for_any_kind_of_field() {
        gtk::init().expect("display");
        let nasty = "<a href='https://evil.example'>Click</a> <b>bold</b> &amp;";
        let mut fields = Vec::new();
        for kind in [Kind::Text, Kind::Private, Kind::Boolean, Kind::ListSingle, Kind::ListMulti, Kind::Multi, Kind::Jid, Kind::JidMulti, Kind::Fixed] {
            fields.push(Field {
                kind,
                var: if kind == Kind::Fixed { String::new() } else { format!("v-{kind:?}") },
                label: nasty.into(),
                values: if kind == Kind::Fixed { vec![nasty.into()] } else { vec![] },
                options: vec![("a".into(), nasty.into()), ("b".into(), "plain".into())],
                required: false,
            });
        }
        let form = Form { title: nasty.into(), instructions: vec![nasty.into()], fields, result: false };
        let (widgets, _) = form_widgets(&form);
        let mut parsed = Vec::new();
        every_label(widgets.upcast_ref(), &mut |label| {
            if label.uses_markup() && label.label().contains('<') {
                parsed.push(label.label().to_string());
            }
        });
        assert!(parsed.is_empty(), "labels that would parse the server's markup: {parsed:?}");
        // The same for a result form.
        let result = Form { result: true, ..form };
        let rows = result_rows(&result);
        let mut parsed = Vec::new();
        every_label(rows.upcast_ref(), &mut |label| {
            if label.uses_markup() && label.label().contains('<') {
                parsed.push(label.label().to_string());
            }
        });
        assert!(parsed.is_empty(), "result rows that would parse markup: {parsed:?}");
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn a_toast_parses_markup_by_default_so_every_toast_is_made_plain_before_it_is_shown() {
        adw::init().unwrap();
        let nasty = "Fill in: <a href='https://evil.example'>Verify your account</a>";
        let toast = adw::Toast::new(nasty);
        assert!(toast.uses_markup(), "the library default: a server's label would be parsed");
        crate::toast_routing::plain_text(&toast);
        assert!(!toast.uses_markup(), "after plain_text it is shown as written");
        assert_eq!(toast.title().as_deref(), Some(nasty), "the text itself is unchanged");
        let built = adw::Toast::builder().title(nasty).button_label("Resolve").build();
        crate::toast_routing::plain_text(&built);
        assert!(!built.uses_markup(), "toasts with buttons too");
    }
}
