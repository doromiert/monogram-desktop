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
    menu: glib::WeakRef<gtk::MenuButton>,
    reorder: Cell<bool>,
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
        let group = gio::SimpleActionGroup::new();
        let menu = gio::Menu::new();
        for (id, title, enabled) in [
            ("reorder", "Reorder", !pack.connected),
            ("rename", "Edit Name", !pack.connected),
            (
                "add",
                "Add Stickers…",
                !pack.connected && !pack.sync_pending,
            ),
            (
                "import",
                "Import Sticker Files…",
                !pack.connected && !pack.sync_pending,
            ),
            ("export", "Export Pack…", !pack.sync_pending),
            ("settings", "Pack Settings", true),
            ("remove", "Remove Pack", true),
        ] {
            let action = gio::SimpleAction::new(id, None);
            action.set_enabled(enabled);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(editor) = weak.upgrade() {
                    match id {
                        "reorder" => {
                            editor.reorder.set(!editor.reorder.get());
                            editor.refresh();
                        }
                        "rename" => editor.rename(),
                        "settings" => editor.settings(),
                        "add" => editor.choose_artwork(None, true),
                        "import" => editor.choose_artwork(None, false),
                        "export" => editor.export(),
                        "remove" => editor.remove_pack(),
                        _ => {}
                    }
                }
            });
            group.add_action(&action);
            menu.append(Some(title), Some(&format!("pack.{id}")));
        }
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.insert_action_group("pack", Some(&group));
        }
        if let Some(button) = self.menu.upgrade() {
            button.set_menu_model(Some(&menu));
        }
        if self.reorder.get() {
            let label = gtk::Label::new(Some(
                "Drag stickers to reorder. Use each sticker’s menu for keyboard moves.",
            ));
            label.set_wrap(true);
            body.append(&label);
        }
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
            let Some(ui) = editor.ui() else {
                return;
            };
            let preview = if pack.sync_pending {
                let cell = gtk::Box::new(gtk::Orientation::Vertical, 0);
                let spinner = gtk::Spinner::new();
                spinner.start();
                spinner.set_tooltip_text(Some("Downloading sticker…"));
                cell.append(&spinner);
                cell
            } else {
                ui.pack_item_preview(item.id, 112, true)
            };
            let tile = gtk::Overlay::new();
            tile.add_css_class("pack-editor-item");
            tile.set_widget_name(&item.id.to_string());
            let button = gtk::Button::new();
            button.add_css_class("flat");
            button.set_child(Some(&crate::widgets::SquareCell::new(&preview)));
            button.set_tooltip_text(Some(&item.name));
            button.set_sensitive(!pack.sync_pending);
            tile.set_child(Some(&button));
            let id = item.id;
            let weak = Rc::downgrade(&editor);
            button.connect_clicked(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.edit_item(id);
                }
            });
            let options = gio::Menu::new();
            let actions = gio::SimpleActionGroup::new();
            for (name, title, to) in [
                ("edit", "Edit Sticker", None),
                ("earlier", "Move Earlier", position.checked_sub(1)),
                (
                    "later",
                    "Move Later",
                    if position + 1 < pack.items.len() {
                        Some(position + 1)
                    } else {
                        None
                    },
                ),
                ("remove", "Remove Sticker", None),
            ] {
                let action = gio::SimpleAction::new(name, None);
                action.set_enabled(
                    !pack.connected && (!matches!(name, "earlier" | "later") || to.is_some()),
                );
                let weak = Rc::downgrade(&editor);
                let from = *position;
                action.connect_activate(move |_, _| {
                    if let Some(editor) = weak.upgrade() {
                        match name {
                            "edit" => editor.edit_item(id),
                            "remove" => editor.mutate(Mutation::RemoveItem {
                                pack: editor.id,
                                item: id,
                            }),
                            _ => {
                                if let Some(to) = to {
                                    editor.mutate(Mutation::MoveItem {
                                        pack: editor.id,
                                        from,
                                        to,
                                    });
                                }
                            }
                        }
                    }
                });
                actions.add_action(&action);
                options.append(Some(title), Some(&format!("item.{name}")));
            }
            tile.insert_action_group("item", Some(&actions));
            let more = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .tooltip_text("Sticker Options")
                .menu_model(&options)
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .build();
            more.add_css_class("osd");
            more.add_css_class("circular");
            tile.add_overlay(&more);
            if editor.reorder.get() && !pack.connected {
                editor.attach_item_drag(&button, item.id);
            }
            list_item.set_child(Some(&tile));
        });
        factory.connect_unbind(|_, object| {
            if let Some(item) = object.downcast_ref::<gtk::ListItem>() {
                item.set_child(gtk::Widget::NONE);
            }
        });
        let list = gtk::GridView::new(Some(selection), Some(factory));
        list.set_min_columns(3);
        list.set_max_columns(5);
        list.add_css_class("pack-editor-grid");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .min_content_height(180)
            .build();
        body.append(&scroll);
    }
    fn attach_item_drag(self: &Rc<Self>, button: &gtk::Button, id: i64) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let active = Rc::new(Cell::new(false));
        let target = Rc::new(Cell::new(None::<i64>));
        let highlight = Rc::new(RefCell::new(None::<glib::WeakRef<gtk::Widget>>));
        let (moving, destination, shown) = (active.clone(), target.clone(), highlight.clone());
        let source = button.downgrade();
        drag.connect_drag_update(move |g, dx, dy| {
            if !moving.get() && dx.hypot(dy) < 8. {
                return;
            }
            let Some(source) = source.upgrade() else {
                return;
            };
            moving.set(true);
            g.set_state(gtk::EventSequenceState::Claimed);
            source.set_cursor_from_name(Some("grabbing"));
            let Some(grid) = source.ancestor(gtk::GridView::static_type()) else {
                return;
            };
            let Some(native) = grid.native() else {
                return;
            };
            let Ok(widget) = native.clone().dynamic_cast::<gtk::Widget>() else {
                return;
            };
            let Some((x, y)) = g.current_event().and_then(|event| event.position()) else {
                return;
            };
            let (tx, ty) = native.surface_transform();
            let Some(point) = widget.compute_point(
                &grid,
                &gtk::graphene::Point::new((x + tx) as f32, (y + ty) as f32),
            ) else {
                return;
            };
            let mut picked = grid.pick(point.x() as f64, point.y() as f64, gtk::PickFlags::DEFAULT);
            while let Some(widget) = picked {
                if widget.has_css_class("pack-editor-item") {
                    if let Ok(next) = widget.widget_name().parse::<i64>() {
                        if let Some(old) = shown.borrow_mut().take().and_then(|w| w.upgrade()) {
                            old.remove_css_class("favorite-drop-target");
                        }
                        widget.add_css_class("favorite-drop-target");
                        shown.replace(Some(widget.downgrade()));
                        destination.set(Some(next));
                    }
                    return;
                }
                picked = widget.parent();
            }
        });
        let weak = Rc::downgrade(self);
        let source = button.downgrade();
        drag.connect_drag_end(move |_, _, _| {
            if let Some(widget) = source.upgrade() {
                widget.set_cursor_from_name(None);
            }
            if let Some(old) = highlight.borrow_mut().take().and_then(|w| w.upgrade()) {
                old.remove_css_class("favorite-drop-target");
            }
            if !active.replace(false) {
                return;
            }
            let (Some(editor), Some(to)) = (weak.upgrade(), target.take()) else {
                return;
            };
            if id == to {
                return;
            }
            let positions = editor.pack.borrow().as_ref().and_then(|p| {
                Some((
                    p.items.iter().position(|i| i.id == id)?,
                    p.items.iter().position(|i| i.id == to)?,
                ))
            });
            if let Some((from, to)) = positions {
                editor.mutate(Mutation::MoveItem {
                    pack: editor.id,
                    from,
                    to,
                });
            }
        });
        button.add_controller(drag);
    }
    fn remove_pack(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        self.feedback("Removing pack…", true);
        let editor = self.clone();
        glib::spawn_future_local(async move {
            match editor
                .library
                .mutate(Mutation::Installed {
                    pack: editor.id,
                    value: false,
                })
                .await
            {
                Ok(_) => {
                    if let Some(page) = editor.page.upgrade() {
                        page.refresh();
                    }
                    if let Some(dialog) = editor.dialog.upgrade() {
                        dialog.close();
                    }
                    if let Some(ui) = editor.ui() {
                        let toast = adw::Toast::builder()
                            .title("Pack removed")
                            .button_label("Undo")
                            .timeout(8)
                            .build();
                        let weak = Rc::downgrade(&ui);
                        let library = editor.library.clone();
                        let page = editor.page.clone();
                        let id = editor.id;
                        toast.connect_button_clicked(move |_| {
                            let (Some(ui), Some(page)) = (weak.upgrade(), page.upgrade()) else {
                                return;
                            };
                            if ui.state.borrow().account != library.account() {
                                return;
                            }
                            let library = library.clone();
                            glib::spawn_future_local(async move {
                                match library
                                    .mutate(Mutation::Installed {
                                        pack: id,
                                        value: true,
                                    })
                                    .await
                                {
                                    Ok(_) => page.refresh(),
                                    Err(error) => ui.notice(&error.to_string()),
                                }
                            });
                        });
                        ui.toast.add_toast(toast);
                    }
                }
                Err(error) => editor.feedback(&error.to_string(), false),
            }
        });
    }
    fn rename(self: &Rc<Self>) {
        let Some(pack) = self.pack.borrow().clone() else {
            return;
        };
        let Some(ui) = self.ui() else {
            return;
        };
        let dialog = adw::AlertDialog::builder()
            .heading("Edit Pack Name")
            .build();
        let name = adw::EntryRow::builder()
            .title("Pack name")
            .text(&pack.name)
            .build();
        dialog.set_extra_child(Some(&name));
        dialog.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(Some("save"), move |_, _| {
            if let Some(editor) = weak.upgrade() {
                editor.mutate(Mutation::Rename {
                    pack: editor.id,
                    name: name.text().into(),
                });
            }
        });
        dialog.present(Some(&ui.window));
    }
    fn settings(self: &Rc<Self>) {
        let Some(pack) = self.pack.borrow().clone() else {
            return;
        };
        let Some(ui) = self.ui() else {
            return;
        };
        let dialog = adw::AlertDialog::builder().heading("Pack Settings").build();
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
        dialog.set_extra_child(Some(&settings));
        dialog.add_response("close", "Close");
        dialog.present(Some(&ui.window));
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
                "replace" => editor.choose_artwork(Some(id), true),
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
    fn choose_artwork(self: &Rc<Self>, replace: Option<i64>, images_only: bool) {
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
            if images_only {
                let filter = gtk::FileFilter::new();
                filter.set_name(Some("Images"));
                filter.add_mime_type("image/*");
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                chooser.set_filters(Some(&filters));
            }
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
    layout.add_css_class("background");
    let header = adw::HeaderBar::new();
    let menu = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Pack Actions")
        .build();
    header.pack_end(&menu);
    layout.append(&header);
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
    let done = gtk::Button::with_label("Done");
    done.add_css_class("suggested-action");
    done.add_css_class("pill");
    crate::widgets::margins(&done, 12);
    let weak = dialog.downgrade();
    done.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
    });
    layout.append(&done);
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
        menu: menu.downgrade(),
        reorder: Cell::new(false),
    });
    let keep = editor.clone();
    dialog.connect_closed(move |_| {
        if let Some(job) = keep.job.borrow_mut().take() {
            job.cancel();
        }
    });
    dialog.present(Some(&ui.window));
    let weak = Rc::downgrade(&editor);
    let seen = Cell::new(crate::lan_sync::REVISION.load(std::sync::atomic::Ordering::Relaxed));
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let Some(editor) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if editor.dialog.upgrade().is_none() || editor.ui().is_none() {
            return glib::ControlFlow::Break;
        }
        let revision = crate::lan_sync::REVISION.load(std::sync::atomic::Ordering::Relaxed);
        if !editor.busy.get() && seen.get() != revision {
            seen.set(revision);
            editor.refresh();
        }
        glib::ControlFlow::Continue
    });
    editor.refresh();
}
