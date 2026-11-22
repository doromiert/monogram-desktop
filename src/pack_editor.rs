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
    serial: Cell<u64>,
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
        if let Some(menu) = self.menu.upgrade() {
            menu.set_sensitive(!busy);
        }
        if let Some(body) = self.body.upgrade() {
            body.set_sensitive(!busy);
        }
    }
    fn refresh(self: &Rc<Self>) {
        let serial = self.serial.get().wrapping_add(1);
        self.serial.set(serial);
        self.feedback("Loading pack…", true);
        let editor = self.clone();
        glib::spawn_future_local(async move {
            let result = editor.library.snapshot().await;
            if editor.serial.get() != serial
                || editor.ui().is_none()
                || editor.dialog.upgrade().is_none()
            {
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
    fn editable(self: &Rc<Self>, run: impl FnOnce(Rc<Self>) + 'static) {
        if !self
            .pack
            .borrow()
            .as_ref()
            .is_some_and(|pack| pack.connected)
        {
            run(self.clone());
            return;
        }
        let Some(ui) = self.ui() else {
            return;
        };
        let dialog=adw::AlertDialog::builder().heading("Edit This Pack Locally?").body("Your changes will be kept on your devices. This disconnects the pack from automatic Telegram updates.").build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("edit", "Edit Locally");
        dialog.set_response_appearance("edit", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("edit"));
        dialog.set_close_response("cancel");
        let editor = self.clone();
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&ui.window)).await.as_str() != "edit" {
                return;
            }
            editor.feedback("Making pack editable…", true);
            match editor
                .library
                .mutate(Mutation::Detach { pack: editor.id })
                .await
            {
                Ok(_) => {
                    if let Some(pack) = editor.pack.borrow_mut().as_mut() {
                        pack.connected = false;
                        pack.auto_sync = false;
                    }
                    editor.feedback("", false);
                    if let Some(page) = editor.page.upgrade() {
                        page.refresh();
                    }
                    run(editor);
                }
                Err(e) => editor.feedback(&e.to_string(), false),
            }
        });
    }
    fn mutate(self: &Rc<Self>, change: Mutation) {
        if self.ui().is_none() {
            return;
        }
        if self.pack.borrow().as_ref().is_some_and(|p| p.connected)
            && matches!(
                change,
                Mutation::Rename { .. }
                    | Mutation::RenameItem { .. }
                    | Mutation::Tag { .. }
                    | Mutation::RemoveItem { .. }
                    | Mutation::MoveItem { .. }
            )
        {
            self.editable(move |editor| editor.mutate(change));
            return;
        }
        self.serial.set(self.serial.get().wrapping_add(1));
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
            ("reorder", "Reorder", !pack.sync_pending),
            ("rename", "Edit Name", !pack.sync_pending),
            ("add", "Add Stickers…", !pack.sync_pending),
            ("import", "Import Sticker Files…", !pack.sync_pending),
            ("export", "Export Pack…", !pack.sync_pending),
            ("settings", "Pack Settings", true),
            ("remove", "Remove Pack", true),
        ] {
            let action = gio::SimpleAction::new(id, None);
            action.set_enabled(enabled);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(editor) = weak.upgrade() {
                    let run = move |editor: Rc<Editor>| match id {
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
                    };
                    if matches!(id, "reorder" | "rename" | "add" | "import") {
                        editor.editable(run);
                    } else {
                        run(editor);
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
        if !pack.installed {
            let install=gtk::Button::with_label("Add Pack");
            install.add_css_class("suggested-action");
            let weak=Rc::downgrade(self);
            install.connect_clicked(move |_| {if let Some(editor)=weak.upgrade(){
                editor.mutate(Mutation::Installed{pack:editor.id,value:true});
            }});
            body.append(&install);
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
                    !pack.sync_pending && (!matches!(name, "earlier" | "later") || to.is_some()),
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
            editor.attach_item_drag(&button, item.id);
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
                        ui.show_toast(toast);
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
        ui.track_dialog(&dialog);
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
            .sensitive(!pack.sync_pending)
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
        ui.track_dialog(&dialog);
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
        ui.track_dialog(&dialog);
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
    header.add_css_class("flat");
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
        serial: Cell::new(0),
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
    ui.track_dialog(&dialog);
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

pub(crate) fn manage(ui: &Rc<Ui>, page: &Rc<PackLibraryPage>, library: Library, kind: packs::Kind) {
    let dialog = adw::Dialog::builder()
        .title("Manage Packs")
        .content_width(520)
        .content_height(620)
        .build();
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    view.add_top_bar(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    crate::widgets::margins(&body, 18);
    let status = gtk::Label::new(Some("Loading packs…"));
    status.set_wrap(true);
    body.append(&status);
    let rows = gtk::ListBox::new();
    rows.set_selection_mode(gtk::SelectionMode::None);
    rows.add_css_class("boxed-list");
    body.append(&rows);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&body)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    view.set_content(Some(&scroll));
    dialog.set_child(Some(&view));
    ui.track_dialog(&dialog);
    dialog.present(Some(&ui.window));
    let owner = library.account().to_string();
    let generation = ui.generation.get();
    let weak_ui = Rc::downgrade(ui);
    let weak_page = Rc::downgrade(page);
    let weak_dialog = dialog.downgrade();
    glib::spawn_future_local(async move {
        let snapshot = library.snapshot().await;
        let Some(_ui) = weak_ui
            .upgrade()
            .filter(|ui| ui.generation.get() == generation && ui.state.borrow().account == owner)
        else {
            return;
        };
        if weak_dialog.upgrade().is_none() {
            return;
        }
        let snapshot = match snapshot {
            Ok(s) => s,
            Err(e) => {
                status.set_text(&e.to_string());
                return;
            }
        };
        status.set_text("Drag packs to reorder them. Use the pencil to edit a pack.");
        let save: Rc<dyn Fn()> = Rc::new({
            let library = library.clone();
            let weak = weak_page.clone();
            let list = rows.downgrade();
            let status = status.downgrade();
            move || {
                let Some(list) = list.upgrade() else {
                    return;
                };
                let mut ids = vec![];
                let mut child = list.first_child();
                while let Some(row) = child {
                    child = row.next_sibling();
                    if let Ok(id) = row.widget_name().parse::<i64>() {
                        ids.push(id);
                    }
                }
                let library = library.clone();
                let weak = weak.clone();
                let status = status.clone();
                glib::spawn_future_local(async move {
                    match library.mutate(Mutation::Reorder { kind, ids }).await {
                        Ok(_) => {
                            if let Some(page) = weak.upgrade() {
                                page.refresh();
                            }
                        }
                        Err(e) => {
                            if let Some(status) = status.upgrade() {
                                status.set_text(&e.to_string());
                            }
                        }
                    }
                });
            }
        });
        for pack in snapshot.packs.into_iter().filter(|p| p.kind == kind) {
            let row = adw::ActionRow::builder()
                .title(&pack.name)
                .use_markup(false)
                .subtitle(format!(
                    "{} items{}",
                    pack.items.len(),
                    if pack.connected {
                        " · Connected to Telegram"
                    } else {
                        ""
                    }
                ))
                .build();
            row.set_widget_name(&pack.id.to_string());
            let edit = crate::widgets::icon_button("document-edit-symbolic", "Edit Pack");
            edit.set_valign(gtk::Align::Center);
            row.add_suffix(&edit);
            let weak = weak_ui.clone();
            let page = weak_page.clone();
            let edit_library = library.clone();
            let id = pack.id;
            edit.connect_clicked(move |_| {
                if let (Some(ui), Some(page)) = (weak.upgrade(), page.upgrade()) {
                    open(&ui, &page, edit_library.clone(), id);
                }
            });
            let installed = gtk::Switch::builder()
                .active(pack.installed)
                .valign(gtk::Align::Center)
                .build();
            installed.update_property(&[gtk::accessible::Property::Label("Installed")]);
            row.add_suffix(&installed);
            let library = library.clone();
            let page = weak_page.clone();
            let feedback = status.downgrade();
            installed.connect_state_set(move |_, value| {
                let library = library.clone();
                let page = page.clone();
                let feedback = feedback.clone();
                glib::spawn_future_local(async move {
                    match library
                        .mutate(Mutation::Installed { pack: id, value })
                        .await
                    {
                        Ok(_) => {
                            if let Some(page) = page.upgrade() {
                                page.refresh();
                            }
                        }
                        Err(e) => {
                            if let Some(label) = feedback.upgrade() {
                                label.set_text(&e.to_string());
                            }
                        }
                    }
                });
                glib::Propagation::Proceed
            });
            let drag = gtk::DragSource::builder()
                .actions(gtk::gdk::DragAction::MOVE)
                .build();
            drag.set_propagation_phase(gtk::PropagationPhase::Capture);drag.set_button(1);
            drag.connect_prepare(move |_, _, _| {
                Some(gtk::gdk::ContentProvider::for_value(
                    &id.to_string().to_value(),
                ))
            });
            drag.connect_drag_begin(|source, _| {
                source.set_icon(Some(&gtk::WidgetPaintable::new(gtk::Widget::NONE)), 0, 0)
            });
            row.add_controller(drag);
            let target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
            let highlight=row.downgrade();
            target.connect_enter(move|_,_,_|{if let Some(row)=highlight.upgrade(){row.add_css_class("favorite-drop-target");}gtk::gdk::DragAction::MOVE});
            let highlight=row.downgrade();target.connect_leave(move|_|{if let Some(row)=highlight.upgrade(){row.remove_css_class("favorite-drop-target");}});
            let list=rows.downgrade();let destination=row.downgrade();let drop_save=save.clone();
            target.connect_drop(move|_,value,_,_|{
                let(Some(list),Some(to),Some(from))=(list.upgrade(),destination.upgrade(),value.get::<String>().ok().and_then(|v|v.parse::<i64>().ok()))else{return false;};
                to.remove_css_class("favorite-drop-target");
                let mut child=list.first_child();while let Some(widget)=child{child=widget.next_sibling();if widget.widget_name()==from.to_string(){if widget==to{return true;}let Ok(source)=widget.downcast::<gtk::ListBoxRow>()else{return false;};let index=to.index();list.remove(&source);list.insert(&source,index);drop_save();return true;}}false
            });
            row.add_controller(target);
            let keys = gtk::EventControllerKey::new();
            let list = rows.downgrade();
            let source = row.downgrade();
            let save = save.clone();
            keys.connect_key_pressed(move |_, key, _, flags| {
                if !flags.contains(gtk::gdk::ModifierType::ALT_MASK) {
                    return glib::Propagation::Proceed;
                }
                let (Some(list), Some(row)) = (list.upgrade(), source.upgrade()) else {
                    return glib::Propagation::Proceed;
                };
                let to = match key {
                    gtk::gdk::Key::Up => row.index() - 1,
                    gtk::gdk::Key::Down => row.index() + 1,
                    _ => return glib::Propagation::Proceed,
                };
                if to >= 0 && list.row_at_index(to).is_some() {
                    list.remove(&row);
                    list.insert(&row, to);
                    row.grab_focus();
                    save();
                }
                glib::Propagation::Stop
            });
            row.add_controller(keys);
            rows.append(&row);
        }
        if rows.first_child().is_none() {
            status.set_text("No packs installed. Import or create a pack to get started.");
        }
    });
}
