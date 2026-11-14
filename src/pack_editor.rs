//! Centered, virtualized pack management using the same operations as mobile.
use crate::{
    media_library_ui::PackLibraryPage,
    packs::{self, Job, Library, Mutation, Pack},
    ui::Ui,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

struct Editor {
    ui: Weak<Ui>,
    page: Weak<PackLibraryPage>,
    library: Library,
    dialog: glib::WeakRef<adw::Dialog>,
    body: glib::WeakRef<gtk::Box>,
    status: glib::WeakRef<gtk::Label>,
    spinner: glib::WeakRef<gtk::Spinner>,
    pack: RefCell<Option<Pack>>,
    id: i64,
    generation: u64,
    busy: Cell<bool>,
    job: RefCell<Option<Job>>,
}
impl Editor {
    fn ui(&self) -> Option<Rc<Ui>> {
        self.ui.upgrade().filter(|ui| {
            ui.generation.get() == self.generation
                && ui.state.borrow().account == self.library.account()
        })
    }
    fn feedback(&self, text: &str, busy: bool) {
        self.busy.set(busy);
        if let Some(status) = self.status.upgrade() {
            status.set_label(text);
            status.set_visible(!text.is_empty());
        }
        if let Some(spinner) = self.spinner.upgrade() {
            spinner.set_spinning(busy);
            spinner.set_visible(busy);
        }
        if let Some(body) = self.body.upgrade() {
            body.set_sensitive(!busy);
        }
    }
    fn refresh(self: &Rc<Self>) {
        self.feedback("Loading pack…", true);
        let editor = self.clone();
        glib::spawn_future_local(async move {
            let result = editor.library.snapshot().await;
            if editor.ui().is_none() || editor.dialog.upgrade().is_none() {
                return;
            }
            match result {
                Ok(snapshot) => {
                    if let Some(pack) = snapshot.packs.into_iter().find(|p| p.id == editor.id) {
                        editor.render(pack);
                        editor.feedback("", false);
                    } else {
                        editor.feedback(
                            "This pack was removed. Close this dialog and choose another pack.",
                            false,
                        );
                    }
                }
                Err(error) => editor.feedback(&error.to_string(), false),
            }
        });
    }
    fn mutate(self: &Rc<Self>, change: Mutation) {
        if self.busy.get() || self.ui().is_none() {
            return;
        }
        self.feedback("Saving…", true);
        let editor = self.clone();
        glib::spawn_future_local(async move {
            match editor.library.mutate(change).await {
                Ok(_) => {
                    if let Some(page) = editor.page.upgrade() {
                        page.refresh();
                    }
                    editor.refresh();
                }
                Err(error) => editor.feedback(&error.to_string(), false),
            }
        });
    }
    fn render(self: &Rc<Self>, pack: Pack) {
        let Some(body) = self.body.upgrade() else {
            return;
        };
        while let Some(child) = body.first_child() {
            body.remove(&child);
        }
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.set_title(&pack.name);
        }
        self.pack.replace(Some(pack.clone()));
        let settings = adw::PreferencesGroup::new();
        let name = adw::EntryRow::builder()
            .title("Pack name")
            .text(&pack.name)
            .show_apply_button(true)
            .sensitive(!pack.connected && !pack.sync_pending)
            .build();
        let weak = Rc::downgrade(self);
        name.connect_apply(move |row| {
            if let Some(editor) = weak.upgrade() {
                editor.mutate(Mutation::Rename {
                    pack: editor.id,
                    name: row.text().into(),
                });
            }
        });
        settings.add(&name);
        for (title, value, kind) in [
            ("Installed", pack.installed, 0),
            ("Private pack", pack.private_pack, 1),
        ] {
            let row = adw::SwitchRow::builder().title(title).active(value).build();
            let weak = Rc::downgrade(self);
            row.connect_active_notify(move |row| {
                if let Some(editor) = weak.upgrade() {
                    let value = row.is_active();
                    editor.mutate(if kind == 0 {
                        Mutation::Installed {
                            pack: editor.id,
                            value,
                        }
                    } else {
                        Mutation::Private {
                            pack: editor.id,
                            value,
                        }
                    });
                }
            });
            settings.add(&row);
        }
        if pack.connected {
            let updates = adw::SwitchRow::builder()
                .title("Update from Telegram automatically")
                .active(pack.auto_sync)
                .build();
            let weak = Rc::downgrade(self);
            updates.connect_active_notify(move |row| {
                if let Some(editor) = weak.upgrade() {
                    editor.mutate(Mutation::AutoSync {
                        pack: editor.id,
                        value: row.is_active(),
                    });
                }
            });
            settings.add(&updates);
            let detach = adw::ActionRow::builder()
                .title("Make an editable local copy")
                .subtitle("Disconnect this pack from Telegram updates")
                .activatable(true)
                .build();
            detach.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = Rc::downgrade(self);
            detach.connect_activated(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.mutate(Mutation::Detach { pack: editor.id });
                }
            });
            settings.add(&detach);
        }
        body.append(&settings);
        let tools = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let add = gtk::Button::with_label("Add Artwork…");
        add.set_sensitive(!pack.connected && !pack.sync_pending);
        let weak = Rc::downgrade(self);
        add.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.choose_artwork(None);
            }
        });
        tools.append(&add);
        let export = gtk::Button::with_label("Export Pack…");
        export.set_sensitive(!pack.sync_pending);
        let weak = Rc::downgrade(self);
        export.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.export();
            }
        });
        tools.append(&export);
        body.append(&tools);
        if pack.sync_pending {
            body.append(&gtk::Label::new(Some(
                "Artwork is downloading. Editing will be available when it finishes.",
            )));
            return;
        }
        let label = gtk::Label::new(Some(&format!("Artwork ({})", pack.items.len())));
        label.set_xalign(0.);
        label.add_css_class("heading");
        body.append(&label);
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        for (position, item) in pack.items.iter().enumerate() {
            model.append(&glib::BoxedAnyObject::new((position, item.clone())));
        }
        let selection = gtk::NoSelection::new(Some(model));
        let factory = gtk::SignalListItemFactory::new();
        let weak = Rc::downgrade(self);
        factory.connect_bind(move |_, object| {
            let (Some(editor), Some(list_item)) =
                (weak.upgrade(), object.downcast_ref::<gtk::ListItem>())
            else {
                return;
            };
            let Some(object) = list_item.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let data = object.borrow::<(usize, packs::Item)>();
            let (position, item) = &*data;
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&item.name).as_str())
                .subtitle(glib::markup_escape_text(&item.emoji).as_str())
                .activatable(!pack.connected)
                .build();
            let weak = Rc::downgrade(&editor);
            let id = item.id;
            row.connect_activated(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.edit_item(id);
                }
            });
            if !pack.connected {
                for (icon, to, enabled) in [
                    ("go-up-symbolic", position.saturating_sub(1), *position > 0),
                    (
                        "go-down-symbolic",
                        position + 1,
                        position + 1 < pack.items.len(),
                    ),
                ] {
                    let button = gtk::Button::from_icon_name(icon);
                    button.add_css_class("flat");
                    button.set_valign(gtk::Align::Center);
                    button.set_sensitive(enabled);
                    button.set_tooltip_text(Some(if to < *position {
                        "Move earlier"
                    } else {
                        "Move later"
                    }));
                    let weak = Rc::downgrade(&editor);
                    let from = *position;
                    button.connect_clicked(move |_| {
                        if let Some(editor) = weak.upgrade() {
                            editor.mutate(Mutation::MoveItem {
                                pack: editor.id,
                                from,
                                to,
                            });
                        }
                    });
                    row.add_suffix(&button);
                }
            }
            list_item.set_child(Some(&row));
        });
        factory.connect_unbind(|_, object| {
            if let Some(item) = object.downcast_ref::<gtk::ListItem>() {
                item.set_child(gtk::Widget::NONE);
            }
        });
        let list = gtk::ListView::new(Some(selection), Some(factory));
        list.add_css_class("boxed-list");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .min_content_height(180)
            .build();
        body.append(&scroll);
    }
    fn edit_item(self: &Rc<Self>, id: i64) {
        let Some(item) = self
            .pack
            .borrow()
            .as_ref()
            .and_then(|p| p.items.iter().find(|i| i.id == id))
            .cloned()
        else {
            return;
        };
        let Some(ui) = self.ui() else {
            return;
        };
        let dialog = adw::AlertDialog::builder().heading("Edit Artwork").build();
        let group = adw::PreferencesGroup::new();
        let name = adw::EntryRow::builder()
            .title("Name")
            .text(&item.name)
            .build();
        let emoji = adw::EntryRow::builder()
            .title("Fallback emoji")
            .text(&item.emoji)
            .build();
        group.add(&name);
        group.add(&emoji);
        dialog.set_extra_child(Some(&group));
        dialog.add_responses(&[
            ("cancel", "Cancel"),
            ("remove", "Remove"),
            ("replace", "Replace Image…"),
            ("save", "Save"),
        ]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(editor) = weak.upgrade() else {
                return;
            };
            match response {
                "remove" => editor.mutate(Mutation::RemoveItem {
                    pack: editor.id,
                    item: id,
                }),
                "replace" => editor.choose_artwork(Some(id)),
                "save" => {
                    if editor.busy.get() {
                        return;
                    }
                    editor.feedback("Saving artwork…", true);
                    let name = name.text().to_string();
                    let emoji = emoji.text().to_string();
                    glib::spawn_future_local(async move {
                        let result = async {
                            editor
                                .library
                                .mutate(Mutation::RenameItem {
                                    pack: editor.id,
                                    item: id,
                                    name,
                                })
                                .await?;
                            editor
                                .library
                                .mutate(Mutation::Tag {
                                    pack: editor.id,
                                    item: id,
                                    emoji,
                                })
                                .await
                        }
                        .await;
                        match result {
                            Ok(_) => {
                                if let Some(page) = editor.page.upgrade() {
                                    page.refresh();
                                }
                                editor.refresh();
                            }
                            Err(error) => editor.feedback(&error.to_string(), false),
                        }
                    });
                }
                _ => {}
            }
        });
        dialog.present(Some(&ui.window));
    }
    fn choose_artwork(self: &Rc<Self>, replace: Option<i64>) {
        if self.busy.get() {
            return;
        }
        let Some(ui) = self.ui() else {
            return;
        };
        let window = ui.window.clone();
        let editor = self.clone();
        glib::spawn_future_local(async move {
            let chooser = gtk::FileDialog::builder()
                .title("Choose Artwork")
                .modal(true)
                .build();
            let files = if replace.is_some() {
                match chooser.open_future(Some(&window)).await {
                    Ok(file) => vec![file],
                    Err(_) => return,
                }
            } else {
                match chooser.open_multiple_future(Some(&window)).await {
                    Ok(files) => files.iter::<gio::File>().flatten().collect(),
                    Err(_) => return,
                }
            };
            if editor.ui().is_none() {
                return;
            }
            let inputs = files
                .into_iter()
                .filter_map(|file| {
                    let path = file.path()?;
                    let name = path.file_stem()?.to_string_lossy().into_owned();
                    Some(packs::Input {
                        path,
                        name,
                        emoji: "🙂".into(),
                    })
                })
                .collect::<Vec<_>>();
            if inputs.is_empty() {
                return;
            }
            editor.feedback("Importing artwork…", true);
            let job = Job::new();
            editor.job.replace(Some(job.clone()));
            let result = if let Some(id) = replace {
                editor
                    .library
                    .replace_item(editor.id, id, inputs.into_iter().next().unwrap(), job)
                    .await
            } else {
                editor.library.add_items(editor.id, inputs, job).await
            };
            editor.job.take();
            match result {
                Ok(_) => {
                    if let Some(page) = editor.page.upgrade() {
                        page.refresh();
                    }
                    editor.refresh();
                }
                Err(error) => editor.feedback(&error.to_string(), false),
            }
        });
    }
    fn export(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let Some(ui) = self.ui() else {
            return;
        };
        let window = ui.window.clone();
        let editor = self.clone();
        glib::spawn_future_local(async move {
            let chooser = gtk::FileDialog::builder()
                .title("Export Pack")
                .initial_name("pack.nzsp")
                .modal(true)
                .build();
            let Ok(file) = chooser.save_future(Some(&window)).await else {
                return;
            };
            let Some(destination) = file.path() else {
                return;
            };
            if editor.ui().is_none() {
                return;
            }
            editor.feedback("Exporting pack…", true);
            let job = Job::new();
            editor.job.replace(Some(job.clone()));
            let result = editor.library.export_xmsp(editor.id, job).await;
            editor.job.take();
            let result = match result {
                Ok(source) => gio::spawn_blocking(move || {
                    let result = std::fs::copy(&source, &destination);
                    let _ = std::fs::remove_file(source);
                    result.map(|_| ()).map_err(
                        |_| "Could not save this file. Choose a writable folder and try again.",
                    )
                })
                .await
                .unwrap_or(Err("Export worker stopped. Try exporting again.")),
                Err(error) => {
                    editor.feedback(&error.to_string(), false);
                    return;
                }
            };
            match result {
                Ok(()) => editor.feedback("Pack exported.", false),
                Err(error) => editor.feedback(error, false),
            }
        });
    }
}
pub(crate) fn open(ui: &Rc<Ui>, page: &Rc<PackLibraryPage>, library: Library, id: i64) {
    let dialog = adw::Dialog::builder()
        .title("Edit Pack")
        .content_width(600)
        .content_height(700)
        .build();
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    layout.append(&adw::HeaderBar::new());
    let feedback = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    feedback.set_margin_start(18);
    feedback.set_margin_end(18);
    let spinner = gtk::Spinner::new();
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.);
    feedback.append(&spinner);
    feedback.append(&status);
    layout.append(&feedback);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 14);
    crate::widgets::margins(&body, 18);
    body.set_vexpand(true);
    layout.append(&body);
    dialog.set_child(Some(&layout));
    let editor = Rc::new(Editor {
        ui: Rc::downgrade(ui),
        page: Rc::downgrade(page),
        library,
        dialog: dialog.downgrade(),
        body: body.downgrade(),
        status: status.downgrade(),
        spinner: spinner.downgrade(),
        pack: RefCell::new(None),
        id,
        generation: ui.generation.get(),
        busy: Cell::new(false),
        job: RefCell::new(None),
    });
    let keep = editor.clone();
    dialog.connect_closed(move |_| {
        if let Some(job) = keep.job.borrow_mut().take() {
            job.cancel();
        }
    });
    dialog.present(Some(&ui.window));
    editor.refresh();
}
