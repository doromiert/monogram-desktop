//! The "Edit Picture" dialog: crop, rotate and mark up a picture attached to the composer.
//!
//! The picture is only ever decoded in sandboxes: the preview by the glycin image loader, the
//! edit itself by the confined GStreamer helper (`media_author_run`). The original file is never
//! modified; the result is a new file under the cache directory that replaces the attachment.
//! Every control works without a pointer: the crop can be nudged with the arrow keys and typed in
//! numerically under "Precise Crop", and "Outline the Crop Area" places a square without dragging.
use crate::{
    crop_area::{CropArea, Tool},
    media_author_run,
    picture_editor::{Aspect, Editor, Kind, Weight, INKS, MAX_STROKE, MIN_EDGE},
    ui::Ui,
    widgets::*,
};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime},
};

/// Longest edge of the preview texture; the crop works in source pixels regardless.
const PREVIEW_EDGE: u32 = 1600;
/// Edited copies are removed after this long, whether or not they were sent.
const KEEP_EDITED: Duration = Duration::from_secs(24 * 60 * 60);

/// Pictures the helper can edit: JPEG, PNG and WebP.
pub fn editable(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, extension)| matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp"))
}

fn edits_root() -> PathBuf {
    glib::user_cache_dir().join("monogram-desktop").join("edited")
}
/// A private directory for edited copies; old ones are pruned each time it is opened.
fn edits_dir() -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = edits_root();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).ok()?;
    prune(&dir, KEEP_EDITED);
    Some(dir)
}
fn prune(dir: &Path, older_than: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > older_than);
        if old && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
/// Whether `path` is a file directly inside the edits directory. Checked on the path's
/// components, not its text, and refusing `.` and `..`: a lexical prefix match would accept
/// `<edits>/../../anything`, which is outside it.
fn is_edited_copy(path: &Path) -> bool {
    path.parent() == Some(edits_root().as_path())
        && path.components().all(|part| matches!(part, std::path::Component::Normal(_) | std::path::Component::RootDir | std::path::Component::Prefix(_)))
}
/// Remove `path` if (and only if) it is one of our edited copies.
pub fn discard_edited(path: Option<PathBuf>) {
    if let Some(path) = path.filter(|path| is_edited_copy(path)) {
        let _ = std::fs::remove_file(path);
    }
}

fn busy_page(text: &str) -> gtk::Box {
    let page = bx(true, 12);
    page.set_valign(gtk::Align::Center);
    page.set_halign(gtk::Align::Center);
    let spinner = adw::Spinner::new();
    spinner.set_size_request(32, 32);
    page.append(&spinner);
    page.append(&label(text, "dim-label"));
    page
}

struct Controls {
    area: CropArea,
    x: adw::SpinRow,
    y: adw::SpinRow,
    width: adw::SpinRow,
    height: adw::SpinRow,
    expander: adw::ExpanderRow,
    aspect: adw::ToggleGroup,
    done: gtk::Button,
    undo: gtk::Button,
    size: gtk::Scale,
    syncing: Cell<bool>,
}
impl Controls {
    /// Put the line size slider (and so the pen) at `pixels`.
    fn set_size(&self, pixels: u32) {
        self.size.set_value(f64::from(pixels));
    }
    /// Show the model's current numbers in the controls without feeding them back.
    fn refresh(&self) {
        let Some(editor) = self.area.editor() else { return };
        self.syncing.set(true);
        let (view_w, view_h) = editor.view_size();
        let rect = editor.rect();
        for (row, value, upper, lower) in [
            (&self.x, rect.x, view_w.saturating_sub(MIN_EDGE.min(view_w)), 0),
            (&self.y, rect.y, view_h.saturating_sub(MIN_EDGE.min(view_h)), 0),
            (&self.width, rect.width, view_w, MIN_EDGE.min(view_w)),
            (&self.height, rect.height, view_h, MIN_EDGE.min(view_h)),
        ] {
            row.adjustment().set_lower(f64::from(lower));
            row.adjustment().set_upper(f64::from(upper));
            row.set_value(f64::from(value));
        }
        self.expander.set_subtitle(&format!("{} × {} px", rect.width, rect.height));
        self.aspect.set_active_name(Some(match editor.aspect() {
            Aspect::Free => "free",
            Aspect::Original => "original",
            Aspect::Square => "square",
            Aspect::Ratio(4, 3) => "4:3",
            Aspect::Ratio(16, 9) => "16:9",
            Aspect::Ratio(..) => "free",
        }));
        let marks = editor.marks().len();
        self.undo.set_sensitive(marks > 0);
        self.undo.set_tooltip_text(Some(&match marks {
            0 => "Undo (nothing drawn yet)".to_string(),
            1 => "Undo the last mark (1 mark)".to_string(),
            n => format!("Undo the last mark ({n} marks)"),
        }));
        self.done.set_sensitive(true);
        self.syncing.set(false);
    }
}

impl Ui {
    /// Open the editor for the picture at `path`. `done` receives the edited copy; it is not
    /// called if the user cancels or changes nothing.
    pub fn open_picture_editor(self: &Rc<Self>, path: PathBuf, done: impl FnOnce(PathBuf) + 'static) {
        let cancel = Arc::new(AtomicBool::new(false));
        let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
        stack.add_named(&busy_page("Opening picture…"), Some("loading"));
        stack.add_named(&busy_page("Applying changes…"), Some("working"));
        let failed = adw::StatusPage::builder().icon_name("dialog-warning-symbolic").title("Can’t Edit This Picture").build();
        stack.add_named(&failed, Some("error"));

        let area = CropArea::new();

        // Drawing tools down the left edge. None selected means the crop frame is what you drag.
        let tool_strip = bx(true, 6);
        tool_strip.set_valign(gtk::Align::Center);
        tool_strip.set_margin_start(6);
        tool_strip.add_css_class("picture-tools");
        let tool_buttons: Vec<(gtk::ToggleButton, Tool)> = [
            ("document-edit-symbolic", "Draw freehand", Tool::Pen),
            ("monogram-shape-circle-symbolic", "Circle", Tool::Circle),
            ("monogram-shape-square-symbolic", "Square", Tool::Square),
            ("monogram-shape-arrow-symbolic", "Arrow", Tool::Arrow),
        ]
        .into_iter()
        .map(|(icon, tip, tool)| {
            let button = gtk::ToggleButton::builder().icon_name(icon).tooltip_text(tip).build();
            button.add_css_class("circular");
            button.update_property(&[gtk::accessible::Property::Label(tip)]);
            tool_strip.append(&button);
            (button, tool)
        })
        .collect();

        // Colour and size sit in the header, as in a drawing app.
        let ink = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::builder().title("Mark Colour").with_alpha(false).build()));
        ink.set_rgba(&gdk::RGBA::new(
            f32::from(INKS[0].1[0]) / 255.,
            f32::from(INKS[0].1[1]) / 255.,
            f32::from(INKS[0].1[2]) / 255.,
            1.,
        ));
        ink.set_tooltip_text(Some("Mark colour"));
        ink.update_property(&[gtk::accessible::Property::Label("Mark colour")]);
        let size = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1., f64::from(MAX_STROKE), 1.);
        size.set_value(12.);
        size.set_draw_value(true);
        size.set_value_pos(gtk::PositionType::Right);
        size.set_width_request(220);
        size.update_property(&[gtk::accessible::Property::Label("Line size in pixels")]);
        let size_box = bx(true, 6);
        margins(&size_box, 12);
        size_box.append(&label("Line size", "heading"));
        size_box.append(&size);
        let size_button = gtk::MenuButton::builder().label("12 px").tooltip_text("Line size").popover(&gtk::Popover::builder().child(&size_box).build()).build();
        let undo = icon_button("edit-undo-symbolic", "Undo (nothing drawn yet)");
        undo.set_sensitive(false);

        // Everything about the frame lives in one popover so the canvas keeps the room.
        let rotate_left = icon_button("object-rotate-left-symbolic", "Rotate left");
        let rotate_right = icon_button("object-rotate-right-symbolic", "Rotate right");
        let reset = gtk::Button::with_label("Reset");
        let aspect = adw::ToggleGroup::new();
        for (name, title) in [("free", "Free"), ("original", "Original"), ("square", "Square"), ("4:3", "4:3"), ("16:9", "16:9")] {
            aspect.add(adw::Toggle::builder().name(name).label(title).build());
        }
        aspect.set_active_name(Some("free"));
        aspect.upcast_ref::<gtk::Widget>().update_property(&[gtk::accessible::Property::Label("Crop shape")]);
        let turns = bx(false, 6);
        turns.append(&rotate_left);
        turns.append(&rotate_right);
        turns.append(&reset);
        let spin = |title: &str| adw::SpinRow::builder().title(title).adjustment(&gtk::Adjustment::new(0., 0., 100_000., 1., 10., 0.)).build();
        let (x, y, width, height) = (spin("Left"), spin("Top"), spin("Width"), spin("Height"));
        let expander = adw::ExpanderRow::builder().title("Precise Crop").build();
        for row in [&x, &y, &width, &height] {
            expander.add_row(row);
        }
        let outline = adw::ActionRow::builder()
            .title("Mark the Crop Area")
            .subtitle("Place the chosen shape around the crop, without a pointer")
            .activatable(true)
            .build();
        let crop_group = adw::PreferencesGroup::new();
        crop_group.add(&expander);
        crop_group.add(&outline);
        let crop_box = bx(true, 12);
        margins(&crop_box, 12);
        crop_box.append(&label("Crop shape", "heading"));
        crop_box.append(&aspect);
        crop_box.append(&turns);
        crop_box.append(&crop_group);
        let crop_scroll = gtk::ScrolledWindow::builder()
            .child(&crop_box)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(420)
            .min_content_width(300)
            .build();
        let crop_button = gtk::MenuButton::builder()
            .label("Crop")
            .tooltip_text("Crop and rotate")
            .popover(&gtk::Popover::builder().child(&crop_scroll).build())
            .build();

        let editor_page = bx(false, 0);
        editor_page.append(&tool_strip);
        area.set_margin_start(6);
        area.set_margin_end(6);
        area.set_margin_bottom(6);
        editor_page.append(&area);
        stack.add_named(&editor_page, Some("editor"));
        stack.set_visible_child_name("loading");

        let cancel_button = gtk::Button::with_label("Cancel");
        let done_button = gtk::Button::with_label("Done");
        done_button.add_css_class("suggested-action");
        done_button.set_sensitive(false);
        // No title: the controls need the width, and the dialog is named by what it shows.
        let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).show_title(false).build();
        header.pack_start(&cancel_button);
        header.pack_start(&ink);
        header.pack_start(&size_button);
        header.pack_start(&undo);
        header.pack_end(&done_button);
        header.pack_end(&crop_button);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&toasts));
        let dialog = adw::Dialog::builder()
            .title("Edit Picture")
            .content_width(640)
            .content_height(520)
            .child(&view)
            .default_widget(&done_button)
            .build();

        let controls = Rc::new(Controls { area: area.clone(), x, y, width, height, expander, aspect: aspect.clone(), done: done_button.clone(), undo: undo.clone(), size: size.clone(), syncing: Cell::new(false) });
        {
            let controls = controls.clone();
            area.connect_changed(move || controls.refresh());
        }
        for row in [&controls.x, &controls.y, &controls.width, &controls.height] {
            let controls = controls.clone();
            row.connect_value_notify(move |_| {
                if controls.syncing.get() {
                    return;
                }
                let value = |row: &adw::SpinRow| row.value().round() as i64;
                let (x, y, w, h) = (value(&controls.x), value(&controls.y), value(&controls.width), value(&controls.height));
                controls.area.edit(|editor| editor.set_rect(x, y, w, h));
            });
        }
        {
            let controls = controls.clone();
            aspect.connect_active_name_notify(move |group| {
                if controls.syncing.get() {
                    return;
                }
                let choice = match group.active_name().as_deref() {
                    Some("original") => Aspect::Original,
                    Some("square") => Aspect::Square,
                    Some("4:3") => Aspect::Ratio(4, 3),
                    Some("16:9") => Aspect::Ratio(16, 9),
                    _ => Aspect::Free,
                };
                controls.area.edit(|editor| editor.set_aspect(choice));
            });
        }
        {
            // The colour and the size feed the pen; an exact size in pixels is used as chosen.
            let pen = {
                let (area, ink, size, size_button) = (area.clone(), ink.clone(), size.clone(), size_button.clone());
                Rc::new(move || {
                    let colour = ink.rgba();
                    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
                    let pixels = size.value().round() as u32;
                    size_button.set_label(&format!("{pixels} px"));
                    area.set_pen([channel(colour.red()), channel(colour.green()), channel(colour.blue())], Weight::Px(pixels));
                })
            };
            pen();
            let (on_colour, on_size) = (pen.clone(), pen.clone());
            ink.connect_rgba_notify(move |_| on_colour());
            size.connect_value_changed(move |_| on_size());
        }
        {
            // The tool buttons act as one group that can also be empty, which means cropping.
            let changing = Rc::new(Cell::new(false));
            let buttons: Rc<Vec<(gtk::ToggleButton, Tool)>> = Rc::new(tool_buttons.clone());
            for (button, tool) in tool_buttons {
                let (area, buttons, changing) = (area.clone(), buttons.clone(), changing.clone());
                button.connect_toggled(move |this| {
                    if changing.get() {
                        return;
                    }
                    changing.set(true);
                    if this.is_active() {
                        for (other, _) in buttons.iter().filter(|(other, _)| other != this) {
                            other.set_active(false);
                        }
                        area.set_tool(tool);
                    } else {
                        area.set_tool(Tool::Crop);
                    }
                    changing.set(false);
                });
            }
        }
        {
            let area = area.clone();
            outline.connect_activated(move |_| {
                let kind = area.tool().kind().unwrap_or(Kind::Square);
                let (color, weight) = area.pen_choice();
                area.edit(|editor| {
                    editor.mark_crop(kind, color, weight);
                });
            });
        }
        {
            let area = area.clone();
            undo.connect_clicked(move |_| area.edit(|editor| {
                editor.undo_mark();
            }));
        }
        for (button, clockwise) in [(&rotate_left, false), (&rotate_right, true)] {
            let area = area.clone();
            button.connect_clicked(move |_| area.edit(|editor| editor.rotate(clockwise)));
        }
        {
            let area = area.clone();
            reset.connect_clicked(move |_| area.edit(Editor::reset));
        }
        {
            let (cancel, dialog) = (cancel.clone(), dialog.downgrade());
            cancel_button.connect_clicked(move |_| {
                cancel.store(true, Ordering::Release);
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
            });
        }
        {
            // Closing any way (Escape, window manager) also stops a running edit.
            let cancel = cancel.clone();
            dialog.connect_closed(move |_| cancel.store(true, Ordering::Release));
        }

        // Probe and preview off the GTK thread: both decode untrusted pixels in sandboxes.
        let source = path.clone();
        let worker_cancel = cancel.clone();
        let loaded = gio::spawn_blocking(move || -> Result<(crate::media_authoring::SourceInfo, gdk::Texture), String> {
            let info = media_author_run::probe(&source, &|| worker_cancel.load(Ordering::Acquire)).map_err(|e| e.to_string())?;
            let input = crate::downloads::VerifiedFile(std::fs::File::open(&source).map_err(|_| "This picture could not be opened")?);
            let mut decoder = crate::image_loader::Decoder::open(input, &gio::Cancellable::new()).map_err(str::to_owned)?;
            let frame = decoder.frame(PREVIEW_EDGE).map_err(str::to_owned)?;
            Ok((info, frame.texture))
        });
        let (weak_controls, weak_stack, weak_failed) = (Rc::downgrade(&controls), stack.downgrade(), failed.downgrade());
        glib::spawn_future_local(async move {
            let result = loaded.await.unwrap_or_else(|_| Err("Editing was interrupted".into()));
            let (Some(controls), Some(stack), Some(failed)) = (weak_controls.upgrade(), weak_stack.upgrade(), weak_failed.upgrade()) else {
                return;
            };
            match result {
                Ok((info, texture)) if preview_matches(&info, &texture) => {
                    controls.area.set_texture(Some(texture));
                    let editor = Editor::new(info.width, info.height);
                    // A sensible first line size for this picture, shown on the slider.
                    let first = editor.stroke_for(Weight::Medium);
                    controls.area.set_editor(editor);
                    controls.set_size(first);
                    controls.refresh();
                    stack.set_visible_child_name("editor");
                    controls.area.grab_focus();
                }
                Ok(_) => {
                    failed.set_description(Some("The preview does not match the picture’s orientation."));
                    stack.set_visible_child_name("error");
                }
                Err(message) => {
                    failed.set_description(Some(&message));
                    stack.set_visible_child_name("error");
                }
            }
        });

        let done_cell = Rc::new(std::cell::RefCell::new(Some(done)));
        {
            let (controls, cancel, dialog, stack, toasts) = (controls.clone(), cancel.clone(), dialog.downgrade(), stack.downgrade(), toasts.downgrade());
            done_button.connect_clicked(move |button| {
                let (Some(editor), Some(stack), Some(dialog)) = (controls.area.editor(), stack.upgrade(), dialog.upgrade()) else {
                    return;
                };
                if editor.is_unchanged() {
                    // Nothing to apply: leave the original attached.
                    dialog.close();
                    return;
                }
                let Some(dir) = edits_dir() else {
                    if let Some(toasts) = toasts.upgrade() {
                        toasts.add_toast(adw::Toast::new("The edited picture could not be saved"));
                    }
                    return;
                };
                button.set_sensitive(false);
                stack.set_visible_child_name("working");
                let (job, source, worker_cancel) = (editor.job(None), path.clone(), cancel.clone());
                let result = gio::spawn_blocking(move || media_author_run::author(&source, &job, &dir, &|| worker_cancel.load(Ordering::Acquire)));
                let (button, stack, dialog, toasts, done) = (button.clone(), stack.downgrade(), dialog.downgrade(), toasts.clone(), done_cell.clone());
                glib::spawn_future_local(async move {
                    let result = result.await;
                    let (Some(stack), Some(dialog)) = (stack.upgrade(), dialog.upgrade()) else {
                        // Closed while working: do not leave an orphaned result behind.
                        if let Ok(Ok(made)) = result {
                            discard_edited(Some(made.path));
                        }
                        return;
                    };
                    match result {
                        Ok(Ok(made)) => {
                            if let Some(done) = done.borrow_mut().take() {
                                done(made.path);
                            }
                            dialog.close();
                        }
                        Ok(Err(media_author_run::Error::Cancelled)) => {}
                        failure => {
                            let message = match failure {
                                Ok(Err(error)) => error.to_string(),
                                _ => "Editing was interrupted".into(),
                            };
                            if let Some(toasts) = toasts.upgrade() {
                                toasts.add_toast(adw::Toast::new(&message));
                            }
                            stack.set_visible_child_name("editor");
                            button.set_sensitive(true);
                        }
                    }
                });
            });
        }
        dialog.present(Some(&self.window));
    }
}

/// The preview and the helper must agree on the picture's shape, or the crop would be drawn
/// over the wrong pixels (for example if one of them ignored a camera's orientation tag).
fn preview_matches(info: &crate::media_authoring::SourceInfo, texture: &gdk::Texture) -> bool {
    let (tw, th) = (f64::from(texture.width()), f64::from(texture.height()));
    let (sw, sh) = (f64::from(info.width), f64::from(info.height));
    tw > 0. && th > 0. && ((tw / th) - (sw / sh)).abs() <= 0.02 * (sw / sh).max(1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/authoring").join(name)
    }
    /// A copy in a scratch directory, so tests never touch the repository fixtures.
    fn copy_of(name: &str, as_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("editor-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(as_name);
        std::fs::copy(fixture(name), &path).unwrap();
        path
    }
    fn settle(milliseconds: u64) {
        let context = glib::MainContext::default();
        let until = std::time::Instant::now() + Duration::from_millis(milliseconds);
        while std::time::Instant::now() < until {
            while context.iteration(false) {}
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn wait(timeout_ms: u64, mut done: impl FnMut() -> bool) -> bool {
        let until = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        while std::time::Instant::now() < until {
            settle(20);
            if done() {
                return true;
            }
        }
        false
    }
    fn find<T: IsA<gtk::Widget>>(root: &gtk::Widget, test: &dyn Fn(&gtk::Widget) -> bool) -> Option<T> {
        if test(root) {
            if let Ok(found) = root.clone().downcast::<T>() {
                return Some(found);
            }
        }
        let mut child = root.first_child();
        while let Some(current) = child {
            if let Some(found) = find::<T>(&current, test) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }
    fn open_ui(id: &str) -> Rc<Ui> {
        adw::init().unwrap();
        let app = adw::Application::builder().application_id(id).flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.set_default_size(1200, 900);
        ui.window.present();
        settle(300);
        ui
    }
    fn dialog_area(ui: &Rc<Ui>) -> Option<CropArea> {
        let dialog = ui.window.visible_dialog()?;
        find::<CropArea>(dialog.child()?.upcast_ref(), &|w| w.is::<CropArea>())
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn the_dialog_crops_and_rotates_a_picture_into_a_new_file_and_leaves_the_original_alone() {
        let ui = open_ui("com.negzero.PictureEditorFixture");
        let source = copy_of("gradient-64x32.png", "photo.png");
        let original = std::fs::read(&source).unwrap();
        let result = Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let sink = result.clone();
        ui.open_picture_editor(source.clone(), move |edited| *sink.borrow_mut() = Some(edited));
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())), "the picture loads");
        let area = dialog_area(&ui).unwrap();
        assert_eq!(area.editor().unwrap().view_size(), (64, 32));

        area.edit(|editor| {
            editor.set_rect(16, 8, 32, 16);
            editor.rotate(true);
        });
        let dialog = ui.window.visible_dialog().unwrap();
        let done = find::<gtk::Button>(dialog.child().unwrap().upcast_ref(), &|w| w.downcast_ref::<gtk::Button>().is_some_and(|b| b.label().as_deref() == Some("Done"))).unwrap();
        assert!(done.is_sensitive());
        done.emit_clicked();
        assert!(wait(20_000, || result.borrow().is_some()), "the edited copy arrives");
        let edited = result.borrow().clone().unwrap();
        assert!(edited.starts_with(edits_root()), "{edited:?} is under the cache directory");
        let head = std::fs::read(&edited).unwrap();
        assert_eq!(crate::media_authoring::sniff_image(&head).unwrap(), (crate::media_authoring::Format::Png, 16, 32), "32x16 cropped, then turned");
        assert_eq!(std::fs::read(&source).unwrap(), original, "the original is untouched");
        settle(300);
        assert!(ui.window.visible_dialog().is_none(), "the dialog closed itself");
        discard_edited(Some(edited));
        ui.window.close();
    }

    /// The preview (glycin) and the helper must agree on a phone photo's orientation, or the crop
    /// would be drawn over the wrong pixels: the fixture is stored 64x32 but tagged to show 32x64.
    fn row_titled(ui: &Rc<Ui>, title: &str) -> adw::ActionRow {
        let dialog = ui.window.visible_dialog().unwrap();
        find::<adw::ActionRow>(dialog.child().unwrap().upcast_ref(), &|w| {
            w.downcast_ref::<adw::ActionRow>().is_some_and(|row| row.title() == title)
        })
        .unwrap_or_else(|| panic!("a row titled {title}"))
    }
    fn tip_starting(ui: &Rc<Ui>, prefix: &str) -> gtk::Button {
        let dialog = ui.window.visible_dialog().unwrap();
        find::<gtk::Button>(dialog.child().unwrap().upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>().is_some_and(|b| b.tooltip_text().is_some_and(|tip| tip.starts_with(prefix)))
        })
        .unwrap_or_else(|| panic!("a button with a tooltip starting {prefix}"))
    }
    fn tool_button(ui: &Rc<Ui>, tip: &str) -> gtk::ToggleButton {
        let dialog = ui.window.visible_dialog().unwrap();
        find::<gtk::ToggleButton>(dialog.child().unwrap().upcast_ref(), &|w| {
            w.downcast_ref::<gtk::ToggleButton>().is_some_and(|b| b.tooltip_text().as_deref() == Some(tip))
        })
        .unwrap_or_else(|| panic!("a tool button {tip}"))
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn the_crop_area_mark_button_and_undo_work_without_a_pointer() {
        let ui = open_ui("com.negzero.PictureEditorMarks");
        let source = copy_of("gradient-64x32.png", "photo.png");
        ui.open_picture_editor(source, |_| {});
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())), "the picture loads");
        let area = dialog_area(&ui).unwrap();
        let (outline, undo) = (row_titled(&ui, "Mark the Crop Area"), tip_starting(&ui, "Undo"));
        assert!(!undo.is_sensitive(), "nothing to undo yet");
        area.edit(|editor| editor.set_rect(8, 4, 40, 20));
        outline.emit_by_name::<()>("activated", &[]);
        let editor = area.editor().unwrap();
        assert_eq!(editor.marks().len(), 1);
        assert_eq!(editor.marks()[0].bounds(), editor.rect(), "the square is exactly around the crop");
        assert!(undo.is_sensitive() && undo.tooltip_text().unwrap().contains("1 mark"));
        outline.emit_by_name::<()>("activated", &[]);
        assert!(undo.tooltip_text().unwrap().contains("2 marks"));
        undo.emit_clicked();
        undo.emit_clicked();
        assert!(area.editor().unwrap().marks().is_empty());
        assert!(!undo.is_sensitive(), "nothing left to undo");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn the_tool_strip_picks_one_tool_at_a_time_and_none_means_cropping() {
        let ui = open_ui("com.negzero.PictureEditorTools");
        ui.open_picture_editor(copy_of("gradient-64x32.png", "photo.png"), |_| {});
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        let area = dialog_area(&ui).unwrap();
        assert_eq!(area.tool(), Tool::Crop, "cropping until a tool is chosen");
        let buttons = [("Draw freehand", Tool::Pen), ("Circle", Tool::Circle), ("Square", Tool::Square), ("Arrow", Tool::Arrow)];
        for (tip, tool) in buttons {
            let button = tool_button(&ui, tip);
            button.set_active(true);
            assert_eq!(area.tool(), tool, "{tip}");
            for (other_tip, _) in buttons.iter().filter(|(other, _)| *other != tip) {
                assert!(!tool_button(&ui, other_tip).is_active(), "{other_tip} lets go when {tip} is chosen");
            }
        }
        tool_button(&ui, "Arrow").set_active(false);
        assert_eq!(area.tool(), Tool::Crop, "clicking the active tool again goes back to the crop");
        assert!(buttons.iter().all(|(tip, _)| !tool_button(&ui, tip).is_active()));
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn the_colour_and_the_size_slider_set_the_pen_and_the_size_button_shows_it() {
        let ui = open_ui("com.negzero.PictureEditorPens");
        ui.open_picture_editor(copy_of("gradient-64x32.png", "photo.png"), |_| {});
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        let area = dialog_area(&ui).unwrap();
        let dialog = ui.window.visible_dialog().unwrap();
        let root: gtk::Widget = dialog.child().unwrap().upcast();
        let colour = find::<gtk::ColorDialogButton>(&root, &|w| w.is::<gtk::ColorDialogButton>()).expect("a colour button");
        let size = find::<gtk::Scale>(&root, &|w| w.is::<gtk::Scale>()).expect("a size slider");
        let size_button = find::<gtk::MenuButton>(&root, &|w| w.downcast_ref::<gtk::MenuButton>().is_some_and(|b| b.tooltip_text().as_deref() == Some("Line size"))).unwrap();
        assert_eq!(colour.rgba().red(), f32::from(INKS[0].1[0]) / 255., "red to begin with");
        assert_eq!(size_button.label().as_deref(), Some(&*format!("{} px", size.value().round() as u32)), "the button names the size");
        size.set_value(30.);
        assert_eq!(size_button.label().as_deref(), Some("30 px"));
        colour.set_rgba(&gdk::RGBA::new(0., 0., 1., 1.));
        tool_button(&ui, "Square").set_active(true);
        area.edit(|editor| {
            editor.mark_crop(Kind::Square, area.pen_choice().0, area.pen_choice().1);
        });
        let mark = area.editor().unwrap().marks()[0].clone();
        assert_eq!(mark.color, [0, 0, 255], "the chosen colour");
        assert_eq!(mark.stroke, 30, "the slider's size, in picture pixels");
        size.set_value(2.);
        area.edit(|editor| {
            editor.mark_crop(Kind::Circle, area.pen_choice().0, area.pen_choice().1);
        });
        assert_eq!(area.editor().unwrap().marks()[1].stroke, 2);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn the_whole_editor_fits_a_small_window_and_keeps_the_canvas_visible() {
        let ui = open_ui("com.negzero.PictureEditorSmall");
        ui.window.set_default_size(360, 520);
        ui.open_picture_editor(copy_of("gradient-64x32.png", "photo.png"), |_| {});
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        settle(300);
        let area = dialog_area(&ui).unwrap();
        let dialog = ui.window.visible_dialog().unwrap();
        let (min_w, _, _, _) = dialog.measure(gtk::Orientation::Horizontal, -1);
        let (min_h, _, _, _) = dialog.measure(gtk::Orientation::Vertical, -1);
        assert!(min_w <= 460, "the dialog needs only {min_w}px of width");
        assert!(min_h <= 420, "and {min_h}px of height");
        assert!(area.width() >= 160 && area.height() >= 140, "the canvas keeps {}x{}", area.width(), area.height());
        // Every control that matters is a header or strip button, not something below the canvas.
        for tip in ["Draw freehand", "Circle", "Square", "Arrow"] {
            assert!(tool_button(&ui, tip).is_mapped(), "{tip} is shown");
        }
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn a_marked_picture_is_saved_with_the_square_in_it_and_the_original_is_untouched() {
        let ui = open_ui("com.negzero.PictureEditorMarkedSave");
        let source = copy_of("gradient-64x32.png", "photo.png");
        let original = std::fs::read(&source).unwrap();
        let result = Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let sink = result.clone();
        ui.open_picture_editor(source.clone(), move |edited| *sink.borrow_mut() = Some(edited));
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        let area = dialog_area(&ui).unwrap();
        area.edit(|editor| {
            editor.add_square(crate::media_authoring::Rect { x: 8, y: 4, width: 20, height: 12 }, [250, 10, 10], crate::picture_editor::Weight::Medium);
        });
        let dialog = ui.window.visible_dialog().unwrap();
        let done = find::<gtk::Button>(dialog.child().unwrap().upcast_ref(), &|w| w.downcast_ref::<gtk::Button>().is_some_and(|b| b.label().as_deref() == Some("Done"))).unwrap();
        assert!(done.is_sensitive(), "a mark alone is a change worth saving");
        done.emit_clicked();
        assert!(wait(20_000, || result.borrow().is_some()), "the edited copy arrives");
        let edited = result.borrow().clone().unwrap();
        let texture = gdk::Texture::from_filename(&edited).unwrap();
        assert_eq!((texture.width(), texture.height()), (64, 32));
        let mut rgba = vec![0u8; 64 * 32 * 4];
        texture.download(&mut rgba, 64 * 4);
        // `Texture::download` writes BGRA, so the channels come out blue first.
        let at = |x: usize, y: usize| [rgba[(y * 64 + x) * 4 + 2], rgba[(y * 64 + x) * 4 + 1], rgba[(y * 64 + x) * 4]];
        for (x, y) in [(8, 4), (27, 4), (8, 15), (27, 15), (17, 4)] {
            let [r, g, b] = at(x, y);
            assert!(r > 240 && g < 20 && b < 20, "({x},{y}) is red, got {:?}", at(x, y));
        }
        let [r, g, b] = at(17, 10);
        assert!(!(r > 240 && g < 20 && b < 20), "the inside of the square is the picture");
        assert_eq!(std::fs::read(&source).unwrap(), original, "the original is untouched");
        settle(300);
        discard_edited(Some(edited));
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn the_preview_and_the_helper_agree_on_exif_orientation() {
        let ui = open_ui("com.negzero.PictureEditorOrientationFixture");
        let source = copy_of("smpte-exif90-64x32.jpg", "phone.jpg");
        ui.open_picture_editor(source, |_| {});
        let loaded = wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some()));
        let message = ui.window.visible_dialog().and_then(|d| d.child()).and_then(|c| find::<adw::StatusPage>(c.upcast_ref(), &|w| w.is::<adw::StatusPage>())).and_then(|p| p.description());
        assert!(loaded, "the editor opened (error page said: {message:?})");
        assert_eq!(dialog_area(&ui).unwrap().editor().unwrap().view_size(), (32, 64));
        ui.window.visible_dialog().unwrap().force_close();
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn a_file_that_is_not_a_picture_shows_an_error_and_an_unchanged_picture_attaches_nothing_new() {
        let ui = open_ui("com.negzero.PictureEditorEdgeFixture");
        let text = copy_of("gradient-64x32.png", "fake.png");
        std::fs::write(&text, b"this is not a picture").unwrap();
        ui.open_picture_editor(text, |_| panic!("nothing was edited"));
        let page = || ui.window.visible_dialog().and_then(|d| d.child()).and_then(|c| find::<adw::StatusPage>(c.upcast_ref(), &|w| w.is::<adw::StatusPage>()));
        assert!(wait(15_000, || page().is_some_and(|p| p.description().is_some())), "an error page explains it");
        assert_eq!(page().unwrap().title(), "Can’t Edit This Picture");
        assert!(dialog_area(&ui).is_some_and(|a| a.editor().is_none()), "there is no editor to work in");
        ui.window.visible_dialog().unwrap().force_close();
        settle(200);

        // Pressing Done without changing anything keeps the original and calls nothing.
        let source = copy_of("gradient-64x32.png", "photo.png");
        ui.open_picture_editor(source, |_| panic!("an unchanged picture is not re-encoded"));
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        let dialog = ui.window.visible_dialog().unwrap();
        let done = find::<gtk::Button>(dialog.child().unwrap().upcast_ref(), &|w| w.downcast_ref::<gtk::Button>().is_some_and(|b| b.label().as_deref() == Some("Done"))).unwrap();
        done.emit_clicked();
        settle(400);
        assert!(ui.window.visible_dialog().is_none(), "the dialog just closed");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn an_edited_copy_takes_the_place_of_its_original_among_the_attachments() {
        let ui = open_ui("com.negzero.AttachmentReplaceFixture");
        let files: Vec<_> = ["a.png", "b.png", "c.png"].iter().map(|n| gio::File::for_path(copy_of("gradient-64x32.png", n))).collect();
        assert!(ui.stage_attachment_files(files.clone()));
        let edited = gio::File::for_path(copy_of("gradient-64x32.png", "b-edited.png"));
        let second_card = ui.chips.first_child().and_then(|c| c.next_sibling()).and_downcast::<gtk::Overlay>();
        ui.replace_attachment(&files[1], second_card, edited.clone());
        let names: Vec<_> = ui.attachments.borrow().iter().map(|f| f.basename().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.png", "b-edited.png", "c.png"], "same position, original gone");
        let mut cards = 0;
        let mut child = ui.chips.first_child();
        while let Some(card) = child {
            cards += 1;
            child = card.next_sibling();
        }
        assert_eq!(cards, 3, "one card per attachment");
        // Replacing something that was removed meanwhile discards the copy instead of attaching it.
        let gone = gio::File::for_path(copy_of("gradient-64x32.png", "gone.png"));
        let before = ui.attachments.borrow().len();
        ui.replace_attachment(&gone, None, edited);
        assert_eq!(ui.attachments.borrow().len(), before);
        ui.window.close();
    }

    #[test]
    fn only_jpeg_png_and_webp_offer_editing() {
        for yes in ["a.jpg", "a.JPEG", "photo.png", "x.webp", "archive.tar.png"] {
            assert!(editable(yes), "{yes}");
        }
        for no in ["a.gif", "a.avif", "a.heic", "a.mp4", "a", "png", "a.png.exe", ".jpg."] {
            assert!(!editable(no), "{no}");
        }
    }

    #[test]
    fn only_files_in_the_edits_directory_are_discarded() {
        let root = edits_root();
        std::fs::create_dir_all(&root).unwrap();
        let ours = root.join(format!("test-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&ours, b"x").unwrap();
        let theirs = std::env::temp_dir().join(format!("keep-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&theirs, b"x").unwrap();
        discard_edited(Some(theirs.clone()));
        assert!(theirs.exists(), "the user's own file is never deleted");
        discard_edited(Some(ours.clone()));
        assert!(!ours.exists());
        discard_edited(None);
        let _ = std::fs::remove_file(theirs);
    }

    #[test]
    fn a_path_that_only_looks_like_it_is_inside_the_edits_directory_is_never_deleted() {
        let root = edits_root();
        std::fs::create_dir_all(&root).unwrap();
        // A real file one level above the edits directory, reachable lexically through "..".
        let victim = root.parent().unwrap().join(format!("victim-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&victim, b"keep me").unwrap();
        let sneaky = root.join("..").join(victim.file_name().unwrap());
        assert!(sneaky.starts_with(&root), "a plain prefix check would accept this");
        assert!(sneaky.exists());
        // Nested paths, dot components and the directory itself are refused too.
        let nested = root.join("sub").join("x.png");
        for path in [&sneaky, &nested, &root.join("."), &root] {
            assert!(!is_edited_copy(path), "{path:?}");
        }
        discard_edited(Some(sneaky));
        assert!(victim.exists(), "the file outside the directory survived");
        std::fs::remove_file(&victim).unwrap();
        assert!(is_edited_copy(&root.join("result.png")));
    }

    #[test]
    fn old_edited_copies_are_pruned_but_recent_ones_stay() {
        let dir = std::env::temp_dir().join(format!("prune-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (old, fresh) = (dir.join("old.png"), dir.join("fresh.png"));
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&fresh, b"x").unwrap();
        let aged = SystemTime::now() - Duration::from_secs(3 * 24 * 60 * 60);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(aged).unwrap();
        prune(&dir, KEEP_EDITED);
        assert!(!old.exists() && fresh.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
