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
/// Edited copies are temporary. They are deleted as soon as they are not needed (sent, removed,
/// replaced, or the conversation moved on); this is only the safety net for one that is forgotten,
/// e.g. after a crash. Long enough to retry a failed upload the same evening.
const KEEP_EDITED: Duration = Duration::from_secs(6 * 60 * 60);

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
/// Remove the edited copy a `file:` URI names, if it is one. Anything else is left alone.
pub fn discard_edited_uri(uri: &str) {
    discard_edited(gio::File::for_uri(uri).path());
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

/// The editor's keyboard shortcuts as (id, what it does, accelerators). Binding and the Shortcuts
/// window both read this, so the two cannot disagree. Alternatives are separated by `|`.
pub(crate) const EDITOR_SHORTCUTS: [(&str, &str, &str); 16] = [
    ("undo", "Undo the last mark", "<Control>z"),
    ("redo", "Redo", "<Control><Shift>z|<Control>y"),
    ("tool-draw", "Draw freehand", "b"),
    ("tool-text", "Text", "t"),
    ("tool-circle", "Circle", "o"),
    ("tool-square", "Square", "r"),
    ("tool-arrow", "Arrow", "a"),
    ("tool-crop", "Crop frame (no drawing tool)", "v"),
    ("crop-menu", "Crop and rotate menu", "c"),
    ("size-menu", "Line size", "s"),
    ("colour", "Mark colour", "k"),
    ("thinner", "Thinner line", "bracketleft"),
    ("thicker", "Thicker line", "bracketright"),
    ("rotate-left", "Rotate left", "<Control>Left"),
    ("rotate-right", "Rotate right", "<Control>Right"),
    ("done", "Done", "<Control>Return"),
];

/// The crop shapes, as (name, icon, full name).
const ASPECT_CHOICES: [(&str, &str, &str); 5] = [
    ("free", "monogram-crop-free-symbolic", "Free"),
    ("original", "monogram-crop-original-symbolic", "Original proportions"),
    ("square", "monogram-crop-square-symbolic", "Square"),
    ("4:3", "monogram-crop-4-3-symbolic", "4:3"),
    ("16:9", "monogram-crop-16-9-symbolic", "16:9"),
];

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
    redo: gtk::Button,
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
            1 => "Undo the last mark (1 mark) · Ctrl+Z".to_string(),
            n => format!("Undo the last mark ({n} marks) · Ctrl+Z"),
        }));
        self.redo.set_visible(editor.can_redo());
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
            ("document-edit-symbolic", "Draw freehand (B)", Tool::Pen),
            ("monogram-text-symbolic", "Text (T)", Tool::Text),
            ("monogram-shape-circle-symbolic", "Circle (O)", Tool::Circle),
            ("monogram-shape-square-symbolic", "Square (R)", Tool::Square),
            ("monogram-shape-arrow-symbolic", "Arrow (A)", Tool::Arrow),
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
        let size_heading = label("Line size", "heading");
        size_box.append(&size_heading);
        size_box.append(&size);
        let size_button = gtk::MenuButton::builder().label("12 px").tooltip_text("Line size").popover(&gtk::Popover::builder().child(&size_box).build()).build();
        let undo = icon_button("edit-undo-symbolic", "Undo (nothing drawn yet)");
        undo.set_sensitive(false);
        // Only there once something has been undone, and gone again when nothing can be redone.
        let redo = icon_button("edit-redo-symbolic", "Redo (Ctrl+Shift+Z)");
        redo.set_visible(false);

        // Everything about the frame lives in one popover so the canvas keeps the room.
        let rotate_left = icon_button("object-rotate-left-symbolic", "Rotate left");
        let rotate_right = icon_button("object-rotate-right-symbolic", "Rotate right");
        let reset = gtk::Button::with_label("Reset");
        let aspect = adw::ToggleGroup::new();
        // Icons, not words: a word has to be cut to fit a small popover, an icon does not. The full
        // name is the tooltip and what a screen reader says.
        for (name, icon, title) in ASPECT_CHOICES {
            aspect.add(adw::Toggle::builder().name(name).icon_name(icon).tooltip(title).build());
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

        let mut ui_tool_buttons: Vec<(gtk::ToggleButton, Tool)> = Vec::new();
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
        header.pack_start(&redo);
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

        let controls = Rc::new(Controls { area: area.clone(), x, y, width, height, expander, aspect: aspect.clone(), done: done_button.clone(), undo: undo.clone(), redo: redo.clone(), size: size.clone(), syncing: Cell::new(false) });
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
        // The slider holds the line size for the drawing tools and the text size for the text tool,
        // each remembered while the other is in use.
        let sizes = Rc::new((Cell::new(12.0_f64), Cell::new(48.0_f64)));
        {
            // The colour and the size feed the pen; an exact size in pixels is used as chosen.
            let pen = {
                let (area, ink, size, size_button, sizes) = (area.clone(), ink.clone(), size.clone(), size_button.clone(), sizes.clone());
                Rc::new(move || {
                    let colour = ink.rgba();
                    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
                    let pixels = size.value().round() as u32;
                    size_button.set_label(&format!("{pixels} px"));
                    if area.tool() == Tool::Text {
                        sizes.1.set(f64::from(pixels));
                    } else {
                        sizes.0.set(f64::from(pixels));
                    }
                    let line = sizes.0.get().round() as u32;
                    area.set_pen([channel(colour.red()), channel(colour.green()), channel(colour.blue())], Weight::Px(line));
                })
            };
            pen();
            let (on_colour, on_size) = (pen.clone(), pen.clone());
            ink.connect_rgba_notify(move |_| on_colour());
            size.connect_value_changed(move |_| on_size());
        }
        {
            // The tool buttons act as one group that can also be empty, which means cropping.
            ui_tool_buttons.extend(tool_buttons.iter().cloned());
            let changing = Rc::new(Cell::new(false));
            let buttons: Rc<Vec<(gtk::ToggleButton, Tool)>> = Rc::new(tool_buttons.clone());
            for (button, tool) in tool_buttons {
                let (area, buttons, changing, sizes) = (area.clone(), buttons.clone(), changing.clone(), sizes.clone());
                let (size, size_button, size_heading) = (size.clone(), size_button.clone(), size_heading.clone());
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
                    // The size slider means text size for the text tool and line size otherwise.
                    let text = area.tool() == Tool::Text;
                    let (line, words) = (sizes.0.get(), sizes.1.get());
                    if text {
                        size.set_range(8., 512.);
                        size.set_value(words);
                    } else {
                        size.set_range(1., f64::from(MAX_STROKE));
                        size.set_value(line);
                    }
                    size_heading.set_label(if text { "Text size" } else { "Line size" });
                    size_button.set_tooltip_text(Some(if text { "Text size" } else { "Line size" }));
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
            let for_undo = area.clone();
            undo.connect_clicked(move |_| for_undo.edit(|editor| {
                editor.undo_mark();
            }));
            let for_redo = area.clone();
            redo.connect_clicked(move |_| for_redo.edit(|editor| {
                editor.redo_mark();
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

        {
            let controller = gtk::ShortcutController::new();
            let bind = |id: &str, action: Box<dyn Fn()>| {
                let (_, _, accelerators) = EDITOR_SHORTCUTS.iter().find(|(name, _, _)| *name == id).expect("a listed shortcut");
                let trigger = gtk::ShortcutTrigger::parse_string(accelerators).expect("a valid accelerator");
                controller.add_shortcut(gtk::Shortcut::new(
                    Some(trigger),
                    Some(gtk::CallbackAction::new(move |_, _| {
                        action();
                        glib::Propagation::Stop
                    })),
                ));
            };
            let area_for = |area: &CropArea| area.clone();
            let (a, c) = (area_for(&area), controls.clone());
            bind("undo", Box::new(move || {
                if c.undo.is_sensitive() {
                    a.edit(|editor| {
                        editor.undo_mark();
                    });
                }
            }));
            let a = area.clone();
            bind("redo", Box::new(move || a.edit(|editor| {
                editor.redo_mark();
            })));
            // A tool key picks the tool, and pressing it again lets go of it, as clicking does.
            let buttons: Rc<Vec<(gtk::ToggleButton, Tool)>> = Rc::new(
                ui_tool_buttons.clone(),
            );
            for (id, tool) in [("tool-draw", Tool::Pen), ("tool-text", Tool::Text), ("tool-circle", Tool::Circle), ("tool-square", Tool::Square), ("tool-arrow", Tool::Arrow)] {
                let buttons = buttons.clone();
                bind(id, Box::new(move || {
                    if let Some((button, _)) = buttons.iter().find(|(_, candidate)| *candidate == tool) {
                        button.set_active(!button.is_active());
                    }
                }));
            }
            let buttons_for_crop = buttons.clone();
            bind("tool-crop", Box::new(move || {
                for (button, _) in buttons_for_crop.iter() {
                    button.set_active(false);
                }
            }));
            let crop_menu = crop_button.clone();
            bind("crop-menu", Box::new(move || crop_menu.popup()));
            let size_menu = size_button.clone();
            bind("size-menu", Box::new(move || size_menu.popup()));
            let colour = ink.clone();
            bind("colour", Box::new(move || {
                colour.activate();
            }));
            let scale = size.clone();
            bind("thinner", Box::new(move || scale.set_value((scale.value() - 1.).max(1.))));
            let scale = size.clone();
            bind("thicker", Box::new(move || scale.set_value((scale.value() + 1.).min(f64::from(MAX_STROKE)))));
            let a = area.clone();
            bind("rotate-left", Box::new(move || a.edit(|editor| editor.rotate(false))));
            let a = area.clone();
            bind("rotate-right", Box::new(move || a.edit(|editor| editor.rotate(true))));
            let finish = done_button.clone();
            bind("done", Box::new(move || {
                if finish.is_sensitive() {
                    finish.emit_clicked();
                }
            }));
            view.add_controller(controller);
        }

        {
            // A click with the text tool opens a small entry there; Enter turns what was typed into
            // text on the picture, in the chosen colour and text size.
            let (area_weak, toasts_weak, sizes) = (area.downgrade(), toasts.downgrade(), sizes.clone());
            area.connect_text_request(move |at, widget_point| {
                let Some(area) = area_weak.upgrade() else { return };
                let entry = gtk::Entry::builder().placeholder_text("Type text, then press Enter").width_chars(24).max_length(crate::picture_editor::MAX_TEXT_CHARS as i32).build();
                entry.update_property(&[gtk::accessible::Property::Label("Text to add")]);
                let popover = gtk::Popover::builder().child(&entry).autohide(true).build();
                popover.set_parent(&area);
                popover.set_pointing_to(Some(&gdk::Rectangle::new(widget_point.0 as i32, widget_point.1 as i32, 1, 1)));
                popover.connect_closed(|popover| {
                    let popover = popover.clone();
                    glib::idle_add_local_once(move || popover.unparent());
                });
                let (toasts_weak, sizes, area_weak) = (toasts_weak.clone(), sizes.clone(), area.downgrade());
                let target = popover.downgrade();
                entry.connect_activate(move |entry| {
                    let (Some(area), Some(popover)) = (area_weak.upgrade(), target.upgrade()) else { return };
                    let words = entry.text().to_string();
                    popover.popdown();
                    if words.trim().is_empty() {
                        return;
                    }
                    let (color, _) = area.pen_choice();
                    let placed = Cell::new(false);
                    if let Some(bitmap) = area.render_text(&words, sizes.1.get().round() as u32) {
                        area.edit(|editor| placed.set(editor.add_text(at, &words, bitmap, color)));
                    }
                    if !placed.get() {
                        if let Some(toasts) = toasts_weak.upgrade() {
                            toasts.add_toast(adw::Toast::new("That text is too long or too large for this picture"));
                        }
                    }
                });
                popover.popup();
                entry.grab_focus();
            });
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
    use crate::picture_editor::ViewShape;

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
            w.downcast_ref::<gtk::ToggleButton>().is_some_and(|b| b.tooltip_text().is_some_and(|t| t.starts_with(tip)))
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

    #[test]
    fn every_crop_shape_and_tool_has_an_icon_file_that_ships_with_the_app() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons/hicolor/scalable/actions");
        let mut icons: Vec<&str> = ASPECT_CHOICES.iter().map(|(_, icon, _)| *icon).collect();
        icons.extend(["monogram-text-symbolic", "monogram-shape-circle-symbolic", "monogram-shape-square-symbolic", "monogram-shape-arrow-symbolic"]);
        for icon in icons {
            let svg = std::fs::read_to_string(dir.join(format!("{icon}.svg"))).unwrap_or_else(|_| panic!("{icon}.svg is missing"));
            assert!(svg.contains("<svg") && svg.contains("viewBox=\"0 0 16 16\""), "{icon} is a 16 px symbolic icon");
            assert!(icon.ends_with("-symbolic"), "{icon} must be symbolic so it follows the theme colour");
        }
    }

    #[test]
    fn the_crop_shapes_have_distinct_names_icons_and_readable_titles() {
        let mut names: Vec<&str> = ASPECT_CHOICES.iter().map(|(name, _, _)| *name).collect();
        let mut icons: Vec<&str> = ASPECT_CHOICES.iter().map(|(_, icon, _)| *icon).collect();
        names.sort();
        icons.sort();
        names.dedup();
        icons.dedup();
        assert_eq!((names.len(), icons.len()), (5, 5));
        assert!(ASPECT_CHOICES.iter().all(|(_, _, title)| title.len() >= 2), "no one-letter titles");
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn the_crop_shapes_are_icons_with_full_names_and_still_pick_the_shape() {
        crate::widgets::install_icons();
        let ui = open_ui("com.negzero.PictureEditorShapes");
        ui.open_picture_editor(copy_of("gradient-64x32.png", "photo.png"), |_| {});
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        let area = dialog_area(&ui).unwrap();
        let dialog = ui.window.visible_dialog().unwrap();
        let group = find::<adw::ToggleGroup>(dialog.child().unwrap().upcast_ref(), &|w| w.is::<adw::ToggleGroup>()).expect("the crop shapes");
        let theme = gtk::IconTheme::for_display(&gdk::Display::default().unwrap());
        for (name, icon, title) in ASPECT_CHOICES {
            let toggle = group.toggle_by_name(name).unwrap_or_else(|| panic!("a {name} toggle"));
            assert_eq!(toggle.icon_name().as_deref(), Some(icon));
            assert!(toggle.label().is_none(), "{name} has no text to cut short");
            assert_eq!(toggle.tooltip().as_str(), title, "the tooltip carries the whole name");
            assert!(theme.has_icon(icon), "{icon} is found by the icon theme");
        }
        group.set_active_name(Some("square"));
        let rect = area.editor().unwrap().rect();
        assert_eq!(rect.width, rect.height, "choosing Square squares the crop");
        group.set_active_name(Some("free"));
        ui.window.close();
    }

    /// Run the shortcut bound to `accelerator` (as written in `EDITOR_SHORTCUTS`) the way a key press would.
    fn press(ui: &Rc<Ui>, accelerator: &str) {
        let dialog = ui.window.visible_dialog().unwrap();
        let view = dialog.child().unwrap();
        let controllers = view.observe_controllers();
        for index in 0..controllers.n_items() {
            let Some(controller) = controllers.item(index).and_downcast::<gtk::ShortcutController>() else { continue };
            for position in 0..controller.n_items() {
                let Some(shortcut) = controller.item(position).and_downcast::<gtk::Shortcut>() else { continue };
                let wanted: Vec<&str> = accelerator.split('|').collect();
                let has = |text: &str| text.split('|').any(|part| wanted.contains(&part));
                if shortcut.trigger().is_some_and(|trigger| has(trigger.to_str().as_str())) {
                    shortcut.action().expect("an action").activate(gtk::ShortcutActionFlags::empty(), &view, None);
                    return;
                }
            }
        }
        panic!("no shortcut bound to {accelerator}");
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn the_shortcuts_are_valid_unique_and_do_not_collide_with_typing_or_the_dialog() {
        gtk::init().ok();
        let mut seen = std::collections::BTreeSet::new();
        for (id, name, accelerators) in EDITOR_SHORTCUTS {
            assert!(name.len() > 3, "{id} needs a readable name");
            for accelerator in accelerators.split('|') {
                assert!(gtk::ShortcutTrigger::parse_string(accelerator).is_some(), "{id}: {accelerator} must parse");
                assert!(seen.insert(accelerator), "{accelerator} is bound twice");
            }
        }
        let ids: std::collections::BTreeSet<_> = EDITOR_SHORTCUTS.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids.len(), EDITOR_SHORTCUTS.len(), "ids are unique");
        // Escape closes the dialog and Return is Done's default; neither may be taken.
        assert!(!seen.contains("Escape") && !seen.contains("Return"));
        // Plain keys are single letters or brackets, so they never hide a Ctrl shortcut of the app.
        for accelerator in &seen {
            let plain = !accelerator.contains('<');
            assert!(!plain || accelerator.len() == 1 || accelerator.starts_with("bracket"), "{accelerator}");
        }
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn redo_appears_only_after_an_undo_and_goes_again_when_nothing_is_left_to_redo() {
        let ui = open_ui("com.negzero.PictureEditorRedo");
        ui.open_picture_editor(copy_of("gradient-64x32.png", "photo.png"), |_| {});
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        let area = dialog_area(&ui).unwrap();
        let (undo, redo) = (tip_starting(&ui, "Undo"), tip_starting(&ui, "Redo"));
        assert!(!redo.is_visible(), "hidden before anything is undone");
        let mark = |area: &CropArea| area.edit(|e| {
            e.add_arrow((2, 2), (40, 20), [230, 40, 40], Weight::Px(2));
        });
        mark(&area);
        mark(&area);
        assert!(!redo.is_visible(), "drawing alone does not show it");
        undo.emit_clicked();
        assert!(redo.is_visible(), "shown after an undo");
        undo.emit_clicked();
        assert!(redo.is_visible());
        redo.emit_clicked();
        assert_eq!(area.editor().unwrap().marks().len(), 1);
        assert!(redo.is_visible(), "one more can be redone");
        redo.emit_clicked();
        assert_eq!(area.editor().unwrap().marks().len(), 2);
        assert!(!redo.is_visible(), "gone once everything is back");
        undo.emit_clicked();
        assert!(redo.is_visible());
        mark(&area);
        assert!(!redo.is_visible(), "drawing something new takes it away");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn the_keyboard_shortcuts_drive_tools_undo_redo_size_and_rotation() {
        let ui = open_ui("com.negzero.PictureEditorKeys");
        ui.open_picture_editor(copy_of("gradient-64x32.png", "photo.png"), |_| {});
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        let area = dialog_area(&ui).unwrap();
        // Tools: each key picks its tool, pressing it again lets go, V goes back to cropping.
        for (key, tool) in [("b", Tool::Pen), ("o", Tool::Circle), ("r", Tool::Square), ("a", Tool::Arrow)] {
            press(&ui, key);
            assert_eq!(area.tool(), tool, "key {key}");
            press(&ui, key);
            assert_eq!(area.tool(), Tool::Crop, "key {key} again");
        }
        press(&ui, "a");
        press(&ui, "v");
        assert_eq!(area.tool(), Tool::Crop, "V drops the drawing tool");
        // Undo and redo.
        area.edit(|e| {
            e.add_arrow((2, 2), (40, 20), [230, 40, 40], Weight::Px(2));
        });
        press(&ui, "<Control>z");
        assert!(area.editor().unwrap().marks().is_empty());
        press(&ui, "<Control><Shift>z|<Control>y");
        assert_eq!(area.editor().unwrap().marks().len(), 1);
        press(&ui, "<Control>z");
        press(&ui, "<Control>z");
        assert!(area.editor().unwrap().marks().is_empty(), "undo with nothing to undo is harmless");
        // Rotation.
        assert_eq!(area.editor().unwrap().view_size(), (64, 32));
        press(&ui, "<Control>Right");
        assert_eq!(area.editor().unwrap().view_size(), (32, 64));
        press(&ui, "<Control>Left");
        assert_eq!(area.editor().unwrap().view_size(), (64, 32));
        // Line size.
        let dialog = ui.window.visible_dialog().unwrap();
        let size = find::<gtk::Scale>(dialog.child().unwrap().upcast_ref(), &|w| w.is::<gtk::Scale>()).unwrap();
        size.set_value(10.);
        press(&ui, "bracketright");
        assert_eq!(size.value(), 11.);
        press(&ui, "bracketleft");
        press(&ui, "bracketleft");
        assert_eq!(size.value(), 9.);
        size.set_value(1.);
        press(&ui, "bracketleft");
        assert_eq!(size.value(), 1., "never below one pixel");
        size.set_value(f64::from(MAX_STROKE));
        press(&ui, "bracketright");
        assert_eq!(size.value(), f64::from(MAX_STROKE), "never above the helper's limit");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn the_done_shortcut_saves_and_the_menus_open_from_the_keyboard() {
        let ui = open_ui("com.negzero.PictureEditorKeysDone");
        let source = copy_of("gradient-64x32.png", "photo.png");
        let result = Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let sink = result.clone();
        ui.open_picture_editor(source, move |edited| *sink.borrow_mut() = Some(edited));
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        let area = dialog_area(&ui).unwrap();
        let dialog = ui.window.visible_dialog().unwrap();
        let root: gtk::Widget = dialog.child().unwrap().upcast();
        let menu = |tip: &str| find::<gtk::MenuButton>(&root, &|w| w.downcast_ref::<gtk::MenuButton>().is_some_and(|b| b.tooltip_text().as_deref() == Some(tip))).unwrap();
        press(&ui, "c");
        settle(200);
        assert!(menu("Crop and rotate").popover().is_some_and(|p| p.is_visible()), "C opens the crop menu");
        menu("Crop and rotate").popdown();
        press(&ui, "s");
        settle(200);
        assert!(menu("Line size").popover().is_some_and(|p| p.is_visible()), "S opens the size slider");
        menu("Line size").popdown();
        area.edit(|e| e.rotate(true));
        press(&ui, "<Control>Return");
        assert!(wait(20_000, || result.borrow().is_some()), "Ctrl+Enter is Done");
        discard_edited(result.borrow().clone());
        settle(300);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn the_app_shortcuts_window_lists_the_editor_keys() {
        let ui = open_ui("com.negzero.PictureEditorKeysWindow");
        ui.shortcuts();
        settle(200);
        let dialog = ui.window.visible_dialog().expect("the shortcuts window");
        assert!(dialog.is::<adw::ShortcutsDialog>());
        ui.window.close();
    }

    fn size_slider(ui: &Rc<Ui>) -> gtk::Scale {
        let dialog = ui.window.visible_dialog().unwrap();
        find::<gtk::Scale>(dialog.child().unwrap().upcast_ref(), &|w| w.is::<gtk::Scale>()).expect("a size slider")
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn the_text_tool_is_in_the_strip_and_the_slider_becomes_a_text_size_while_it_is_chosen() {
        let ui = open_ui("com.negzero.PictureEditorTextTool");
        ui.open_picture_editor(copy_of("gradient-64x32.png", "photo.png"), |_| {});
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        let area = dialog_area(&ui).unwrap();
        let size = size_slider(&ui);
        let dialog = ui.window.visible_dialog().unwrap();
        let root: gtk::Widget = dialog.child().unwrap().upcast();
        let heading = |text: &str| find::<gtk::Label>(&root, &|w| w.downcast_ref::<gtk::Label>().is_some_and(|l| l.label() == text));
        size.set_value(20.);
        assert!(heading("Line size").is_some());
        tool_button(&ui, "Text").set_active(true);
        assert_eq!(area.tool(), Tool::Text);
        assert!(heading("Text size").is_some() && heading("Line size").is_none(), "the slider is named for what it sets");
        let (low, high) = (size.adjustment().lower(), size.adjustment().upper());
        assert_eq!((low, high), (8., 512.), "text sizes are larger than line sizes");
        size.set_value(72.);
        tool_button(&ui, "Square").set_active(true);
        assert_eq!((size.adjustment().lower(), size.adjustment().upper()), (1., f64::from(MAX_STROKE)));
        assert_eq!(size.value(), 20., "the line size is where it was left");
        tool_button(&ui, "Text").set_active(true);
        assert_eq!(size.value(), 72., "and so is the text size");
        tool_button(&ui, "Text").set_active(false);
        assert_eq!(size.value(), 20.);
        assert!(heading("Line size").is_some());
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn typing_in_the_text_popover_places_the_words_and_a_blank_or_huge_entry_does_not() {
        let ui = open_ui("com.negzero.PictureEditorTextEntry");
        ui.window.set_default_size(900, 700);
        ui.open_picture_editor(copy_of("gradient-64x32.png", "photo.png"), |_| {});
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        settle(300);
        let area = dialog_area(&ui).unwrap();
        press(&ui, "t");
        assert_eq!(area.tool(), Tool::Text, "T picks the text tool");
        let entry = |area: &CropArea| find::<gtk::Entry>(area.upcast_ref(), &|w| w.is::<gtk::Entry>());
        let type_in = |words: &str| {
            assert!(area.request_text((4, 4)), "a click asks for text");
            let field = entry(&area).expect("an entry opened");
            field.set_text(words);
            field.emit_activate();
            settle(200);
        };
        size_slider(&ui).set_value(10.);
        type_in("Hi");
        assert_eq!(area.editor().unwrap().marks().len(), 1, "the words were placed");
        let ViewShape::Text { text, rect, .. } = area.editor().unwrap().marks()[0].shape.clone() else { panic!("a text mark") };
        assert_eq!(text, "Hi");
        assert!(rect.width > 4 && rect.height > 4 && rect.x <= 4 && rect.y <= 4 + 1, "{rect:?}");
        type_in("   ");
        assert_eq!(area.editor().unwrap().marks().len(), 1, "blanks add nothing");
        size_slider(&ui).set_value(500.);
        type_in("Far too big for a 64 by 32 picture");
        assert_eq!(area.editor().unwrap().marks().len(), 1, "text larger than the picture is refused");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display, glycin loaders, the built helper and bubblewrap"]
    fn text_is_saved_into_the_picture_and_the_original_is_untouched() {
        let ui = open_ui("com.negzero.PictureEditorTextSave");
        let source = copy_of("gradient-64x32.png", "photo.png");
        let original = std::fs::read(&source).unwrap();
        let result = Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let sink = result.clone();
        ui.open_picture_editor(source.clone(), move |edited| *sink.borrow_mut() = Some(edited));
        assert!(wait(15_000, || dialog_area(&ui).is_some_and(|a| a.editor().is_some())));
        settle(300);
        let area = dialog_area(&ui).unwrap();
        press(&ui, "t");
        size_slider(&ui).set_value(20.);
        assert!(area.request_text((2, 2)));
        let field = find::<gtk::Entry>(area.upcast_ref(), &|w| w.is::<gtk::Entry>()).expect("an entry");
        field.set_text("Hi");
        field.emit_activate();
        settle(200);
        let rect = area.editor().unwrap().marks()[0].bounds();
        let dialog = ui.window.visible_dialog().unwrap();
        let done = find::<gtk::Button>(dialog.child().unwrap().upcast_ref(), &|w| w.downcast_ref::<gtk::Button>().is_some_and(|b| b.label().as_deref() == Some("Done"))).unwrap();
        assert!(done.is_sensitive(), "text alone is a change worth saving");
        done.emit_clicked();
        assert!(wait(20_000, || result.borrow().is_some()));
        let edited = result.borrow().clone().unwrap();
        let (before, after) = (gdk::Texture::from_filename(&source).unwrap(), gdk::Texture::from_filename(&edited).unwrap());
        assert_eq!((after.width(), after.height()), (64, 32));
        let read = |texture: &gdk::Texture| {
            let mut data = vec![0u8; 64 * 32 * 4];
            texture.download(&mut data, 64 * 4);
            data
        };
        let (a, b) = (read(&before), read(&after));
        let at = |data: &[u8], x: u32, y: u32| {
            let i = ((y * 64 + x) * 4) as usize;
            [data[i + 2], data[i + 1], data[i]]
        };
        let mut white = 0; // pixels of the pen colour
        let mut changed_outside = 0;
        for y in 0..32 {
            for x in 0..64 {
                let inside = x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
                // The text takes the pen colour, red to begin with: solid letter pixels are red.
                let [r, g, bl] = at(&b, x, y);
                if inside && r >= 225 && g <= 50 && bl <= 50 && at(&a, x, y) != at(&b, x, y) {
                    white += 1;
                }
                if !inside && at(&a, x, y) != at(&b, x, y) {
                    changed_outside += 1;
                }
            }
        }
        assert!(white >= 10, "red letters inside the box: {white} pixels");
        assert_eq!(changed_outside, 0, "nothing changed outside the text's box");
        assert_eq!(std::fs::read(&source).unwrap(), original, "the original is untouched");
        settle(300);
        discard_edited(Some(edited));
        ui.window.close();
    }

    #[test]
    fn a_file_uri_discards_only_our_edited_copies() {
        let root = edits_root();
        std::fs::create_dir_all(&root).unwrap();
        let ours = root.join(format!("authored-uri-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&ours, b"x").unwrap();
        let theirs = std::env::temp_dir().join(format!("keep-uri-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&theirs, b"x").unwrap();
        let uri = |path: &Path| gio::File::for_path(path).uri().to_string();
        discard_edited_uri(&uri(&theirs));
        discard_edited_uri("https://example.org/picture.png");
        discard_edited_uri("not a uri at all");
        discard_edited_uri("");
        assert!(theirs.exists(), "a file of the user's is never deleted");
        // A path that climbs out of the folder, written as a URI, is not ours either.
        let climbing = format!("{}/../{}", uri(&root), theirs.file_name().unwrap().to_string_lossy());
        discard_edited_uri(&climbing);
        assert!(theirs.exists());
        assert!(ours.exists(), "nothing of ours was touched by those");
        discard_edited_uri(&uri(&ours));
        assert!(!ours.exists(), "ours goes");
        discard_edited_uri(&uri(&ours));
        let _ = std::fs::remove_file(theirs);
    }

    #[test]
    fn a_forgotten_edited_copy_is_swept_within_hours_not_days() {
        assert!(KEEP_EDITED >= Duration::from_secs(60 * 60), "long enough to retry a failed upload");
        assert!(KEEP_EDITED <= Duration::from_secs(12 * 60 * 60), "but temporary");
    }
}
