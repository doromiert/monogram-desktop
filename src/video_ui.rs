//! Native playback of authenticated cached media. The open descriptor pins the verified bytes.
use crate::{downloads::VerifiedFile, model::Message, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    cell::{Cell, RefCell},
    os::fd::AsRawFd,
    rc::Rc,
    time::{Duration, Instant},
};

struct Playback {
    media: gtk::MediaFile,
    _input: VerifiedFile,
    expanded: Cell<bool>,
}
impl Playback {
    fn new(input: VerifiedFile) -> Rc<Self> {
        let file = gio::File::for_path(format!("/proc/self/fd/{}", input.0.as_raw_fd()));
        let media = gtk::MediaFile::for_file(&file);
        media.set_muted(true);
        media.set_loop(true);
        Rc::new(Self {
            media,
            _input: input,
            expanded: Cell::new(false),
        })
    }
}
impl Drop for Playback {
    fn drop(&mut self) {
        self.media.pause();
        self.media.clear();
    }
}
impl Ui {
    pub fn attach_video(
        self: &Rc<Self>,
        message: &Message,
        content: &gtk::Box,
        controls: &gtk::Box,
    ) {
        let file = message.attachment.as_ref().unwrap().clone();
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let image = AspectImage::thumbnail(320, false);
        let button = gtk::Button::builder()
            .child(&image)
            .visible(false)
            .halign(gtk::Align::Start)
            .tooltip_text("Play video")
            .build();
        button.add_css_class("shared-media-tile");
        button.add_css_class("message-media-tile");
        let playback: Rc<RefCell<Option<Rc<Playback>>>> = Rc::new(RefCell::new(None));
        let playing = playback.clone();
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let (Some(ui), Some(playback)) = (weak.upgrade(), playing.borrow().clone()) {
                ui.expand_video(playback);
            }
        });
        content.append(&button);
        let pending = Rc::new(Cell::new(false));
        let weak = Rc::downgrade(self);
        let view = image.downgrade();
        let tile = button.downgrade();
        let fallback = controls.downgrade();
        let load = move || {
            let (Some(ui), Some(image), Some(button), Some(controls)) = (
                weak.upgrade(),
                view.upgrade(),
                tile.upgrade(),
                fallback.upgrade(),
            ) else {
                return glib::ControlFlow::Break;
            };
            if ui.state.borrow().account != owner || ui.generation.get() != generation {
                return glib::ControlFlow::Break;
            }
            if let Some(playback) = playback.borrow().as_ref() {
                if playback.media.error().is_some() {
                    clear(&controls);
                    controls.append(&wrap("Video playback unavailable", "dim-label"));
                    controls.set_visible(true);
                    button.set_visible(false);
                    return glib::ControlFlow::Break;
                }
                if playback.media.is_prepared() {
                    if !playback.media.has_video() {
                        clear(&controls);
                        controls.append(&wrap("This attachment has no video track", "dim-label"));
                        controls.set_visible(true);
                        return glib::ControlFlow::Break;
                    }
                    image.set_paintable(Some(playback.media.clone().upcast()));
                    button.set_visible(true);
                    controls.set_visible(false);
                    if !playback.expanded.get() {
                        playback.media.set_playing(ui.in_chat_viewport(&button));
                    }
                }
                return glib::ControlFlow::Continue;
            }
            if pending.get() {
                return glib::ControlFlow::Continue;
            }
            let Some(path) = ui.cached_image_source(&file) else {
                return glib::ControlFlow::Continue;
            };
            let Some(cancel) = ui.downloads.borrow().token(&owner, &file) else {
                return glib::ControlFlow::Continue;
            };
            pending.set(true);
            let source = file.clone();
            let weak = weak.clone();
            let owner = owner.clone();
            let view = view.clone();
            let fallback = fallback.clone();
            let playback = playback.clone();
            glib::MainContext::default().spawn_local(async move {
                let verified = gio::spawn_blocking(move || {
                    crate::downloads::verified_input(&path, &source, &cancel)
                })
                .await;
                let Some(ui) = weak.upgrade().filter(|ui| {
                    ui.state.borrow().account == owner && ui.generation.get() == generation
                }) else {
                    return;
                };
                let Some(image) = view.upgrade() else { return };
                match verified {
                    Ok(Ok(input)) => {
                        let player = Playback::new(input);
                        let picture = image.downgrade();
                        player.media.connect_invalidate_contents(move |_| {
                            if let Some(image) = picture.upgrade() {
                                image.queue_draw();
                            }
                        });
                        let picture = image.downgrade();
                        player.media.connect_invalidate_size(move |media| {
                            if let Some(image) = picture.upgrade() {
                                image.set_paintable(Some(media.clone().upcast()));
                            }
                        });
                        playback.replace(Some(player));
                    }
                    _ => {
                        if let Some(controls) = fallback.upgrade() {
                            clear(&controls);
                            controls
                                .append(&wrap("Video failed integrity verification", "dim-label"));
                        }
                        ui.notice("Download the video again");
                    }
                }
            });
            glib::ControlFlow::Continue
        };
        if load() == glib::ControlFlow::Continue {
            glib::timeout_add_local(Duration::from_millis(150), load);
        }
    }

    fn expand_video(self: &Rc<Self>, playback: Rc<Playback>) {
        if playback.expanded.replace(true) {
            return;
        }
        if let Some(viewer) = self.active_media.borrow().as_ref() {
            viewer.close_immediate();
        }
        let root = gtk::Overlay::builder()
            .hexpand(true)
            .vexpand(true)
            .focusable(true)
            .build();
        root.add_css_class("media-viewer");
        let picture = gtk::Picture::for_paintable(&playback.media);
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_shrink(true);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        root.set_child(Some(&picture));
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .valign(gtk::Align::Start)
            .title_widget(&label("Video", "heading"))
            .build();
        header.add_css_class("media-controls-top");
        let back = icon_button("go-previous-symbolic", "Back");
        header.pack_start(&back);
        let fullscreen = icon_button("view-fullscreen-symbolic", "Fullscreen");
        header.pack_end(&fullscreen);
        let top = gtk::Revealer::builder()
            .child(&header)
            .reveal_child(true)
            .valign(gtk::Align::Start)
            .transition_duration(180)
            .build();
        root.add_overlay(&top);
        let media_controls = gtk::MediaControls::new(Some(&playback.media));
        margins(&media_controls, 12);
        let bottom = gtk::Revealer::builder()
            .child(&media_controls)
            .reveal_child(true)
            .valign(gtk::Align::End)
            .transition_duration(180)
            .build();
        root.add_overlay(&bottom);
        let was_fullscreen = self.window.is_fullscreen();
        let closing = Rc::new(Cell::new(false));
        let animation_state: Rc<RefCell<Option<adw::TimedAnimation>>> = Rc::new(RefCell::new(None));
        let running = animation_state.clone();
        root.connect_unrealize(move |_| {
            if let Some(animation) = running.borrow_mut().take() {
                animation.pause();
            }
        });
        let animations = animation_state.clone();
        let weak = Rc::downgrade(self);
        let overlay = root.downgrade();
        let player = playback.clone();
        let close: Rc<dyn Fn()> = Rc::new(move || {
            if closing.replace(true) {
                return;
            }
            player.media.pause();
            player.media.set_muted(true);
            player.expanded.set(false);
            if let Some(animation) = animations.borrow_mut().take() {
                animation.pause();
            }
            let (Some(ui), Some(root)) = (weak.upgrade(), overlay.upgrade()) else {
                return;
            };
            if !was_fullscreen {
                ui.window.unfullscreen();
            }
            let widget = root.downgrade();
            let animation = adw::TimedAnimation::new(
                &root,
                1.,
                0.,
                180,
                adw::CallbackAnimationTarget::new(move |value| {
                    if let Some(root) = widget.upgrade() {
                        root.set_opacity(value);
                    }
                }),
            );
            let weak = Rc::downgrade(&ui);
            let widget = root.downgrade();
            let completed = animations.clone();
            animation.connect_done(move |_| {
                completed.borrow_mut().take();
                if let (Some(ui), Some(root)) = (weak.upgrade(), widget.upgrade()) {
                    ui.overlay.remove_overlay(&root);
                }
            });
            animations.replace(Some(animation.clone()));
            animation.play();
        });
        let finish = close.clone();
        back.connect_clicked(move |_| finish());
        let window = self.window.downgrade();
        fullscreen.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                if window.is_fullscreen() {
                    window.unfullscreen();
                } else {
                    window.fullscreen();
                }
            }
        });
        let last_input = Rc::new(Cell::new(Instant::now()));
        let input = last_input.clone();
        let t = top.downgrade();
        let b = bottom.downgrade();
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(move |_, _, _| {
            input.set(Instant::now());
            if let Some(top) = t.upgrade() {
                top.set_reveal_child(true);
            }
            if let Some(bottom) = b.upgrade() {
                bottom.set_reveal_child(true);
            }
        });
        root.add_controller(motion);
        let input = last_input.clone();
        let t = top.downgrade();
        let b = bottom.downgrade();
        let window = self.window.downgrade();
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        let owner = self.state.borrow().account.clone();
        let peer = self.state.borrow().current().jid.clone();
        let finish = close.clone();
        glib::timeout_add_local(Duration::from_millis(200), move || {
            let (Some(top), Some(bottom), Some(window)) =
                (t.upgrade(), b.upgrade(), window.upgrade())
            else {
                return glib::ControlFlow::Break;
            };
            if weak.upgrade().is_none_or(|ui| {
                ui.generation.get() != generation
                    || ui.state.borrow().account != owner
                    || ui.state.borrow().current().jid != peer
            }) {
                finish();
                return glib::ControlFlow::Break;
            }
            let visible = !window.is_fullscreen() || input.get().elapsed() < Duration::from_secs(3);
            top.set_reveal_child(visible);
            bottom.set_reveal_child(visible);
            glib::ControlFlow::Continue
        });
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        let finish = close.clone();
        let overlay = root.downgrade();
        let image = picture.downgrade();
        let input = last_input.clone();
        click.connect_released(move |_, _, x, y| {
            input.set(Instant::now());
            let (Some(root), Some(picture)) = (overlay.upgrade(), image.upgrade()) else {
                return;
            };
            let mut target = root.pick(x, y, gtk::PickFlags::DEFAULT);
            while let Some(widget) = target {
                if widget.is::<gtk::Button>()
                    || widget.is::<gtk::MediaControls>()
                    || widget.is::<adw::HeaderBar>()
                {
                    return;
                }
                target = widget.parent();
            }
            if let Some(bounds) = picture.compute_bounds(&root) {
                let width = bounds.width() as f64;
                let height = bounds.height() as f64;
                let ratio = picture
                    .paintable()
                    .map_or(1., |image| image.intrinsic_aspect_ratio())
                    .max(0.01);
                let (w, h) = (width.min(height * ratio), height.min(width / ratio));
                if (x - bounds.x() as f64 - width / 2.).abs() > w / 2.
                    || (y - bounds.y() as f64 - height / 2.).abs() > h / 2.
                {
                    finish();
                }
            }
        });
        root.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        let finish = close;
        let player = playback.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            last_input.set(Instant::now());
            match key {
                gdk::Key::Escape => finish(),
                gdk::Key::space => player.media.set_playing(!player.media.is_playing()),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        root.add_controller(keys);
        playback.media.set_muted(false);
        playback.media.play();
        self.overlay.add_overlay(&root);
        root.grab_focus();
        root.set_opacity(0.);
        let overlay = root.downgrade();
        let animation = adw::TimedAnimation::new(
            &root,
            0.,
            1.,
            180,
            adw::CallbackAnimationTarget::new(move |value| {
                if let Some(root) = overlay.upgrade() {
                    root.set_opacity(value);
                }
            }),
        );
        animation_state.replace(Some(animation.clone()));
        animation.play();
    }
}
