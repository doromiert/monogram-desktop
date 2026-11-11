//! Native playback of authenticated cached media. The open descriptor pins the verified bytes.
use crate::{downloads::VerifiedFile, model::Message, ui::Ui, widgets::*};
use adw::prelude::*;
use glib::translate::{from_glib_full, from_glib_none};
use gtk::{gio, glib};
use std::ffi::CString;

unsafe extern "C" {
    fn monogram_playback_init() -> i32;
    fn monogram_video_new(
        uri: *const std::ffi::c_char,
        controls: *mut *mut gtk::ffi::GtkWidget,
        replay: *mut *mut gtk::ffi::GtkWidget,
    ) -> *mut gtk::ffi::GtkMediaStream;
}
use std::{
    cell::Cell,
    os::fd::AsRawFd,
    rc::Rc,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

pub(crate) struct Playback {
    pub(crate) media: gtk::MediaStream,
    pub(crate) controls: gtk::Widget,
    pub(crate) replay: gtk::Button,
    _input: VerifiedFile,
    pub(crate) expanded: Cell<bool>,
    inline_users: Cell<usize>,
}
impl Playback {
    pub(crate) fn new(input: VerifiedFile) -> Option<Rc<Self>> {
        let file = gio::File::for_path(format!("/proc/self/fd/{}", input.0.as_raw_fd()));
        let uri = CString::new(file.uri().as_str()).ok()?;
        let mut controls = std::ptr::null_mut();
        let mut replay = std::ptr::null_mut();
        let pointer = unsafe { monogram_video_new(uri.as_ptr(), &mut controls, &mut replay) };
        if pointer.is_null() {
            return None;
        }
        let media: gtk::MediaStream = unsafe { from_glib_full(pointer) };
        let controls: gtk::Widget = unsafe { from_glib_none(controls) };
        let replay: gtk::Button = unsafe { from_glib_none(replay.cast::<gtk::ffi::GtkButton>()) };
        media.set_muted(true);
        media.set_loop(true);
        Some(Rc::new(Self {
            media,
            controls,
            replay,
            _input: input,
            expanded: Cell::new(false),
            inline_users: Cell::new(0),
        }))
    }
}
impl Drop for Playback {
    fn drop(&mut self) {
        self.media.pause();
    }
}
struct InlinePlayback {
    player: Rc<Playback>,
    signals: Vec<glib::SignalHandlerId>,
    visible: Cell<bool>,
}
impl InlinePlayback {
    fn new(player: Rc<Playback>, image: &AspectImage) -> Self {
        let picture = image.downgrade();
        let contents = player.media.connect_invalidate_contents(move |_| {
            if let Some(image) = picture.upgrade() {
                image.queue_draw();
            }
        });
        let picture = image.downgrade();
        let size = player.media.connect_invalidate_size(move |media| {
            if let Some(image) = picture.upgrade() {
                image.set_paintable(Some(media.clone().upcast()));
            }
        });
        Self {
            player,
            signals: vec![contents, size],
            visible: Cell::new(false),
        }
    }
    fn visible(&self, visible: bool) {
        if self.visible.replace(visible) != visible {
            let users = self.player.inline_users.get();
            self.player.inline_users.set(if visible {
                users + 1
            } else {
                users.saturating_sub(1)
            });
        }
        if !self.player.expanded.get() {
            let playing = self.player.inline_users.get() > 0;
            if self.player.media.is_playing() != playing {
                self.player.media.set_playing(playing);
            }
        }
    }
}
impl Drop for InlinePlayback {
    fn drop(&mut self) {
        self.visible(false);
        for signal in self.signals.drain(..) {
            self.player.media.disconnect(signal);
        }
    }
}
impl Ui {
    pub(crate) fn video(
        self: &Rc<Self>,
        file: &crate::file_share::Attachment,
        path: &std::path::Path,
    ) -> Option<Rc<Playback>> {
        let key = file.key();
        let owner = self.state.borrow().account.clone();
        let cancel = self.downloads.borrow().token(&owner, file)?;
        {
            let mut images = self.images.borrow_mut();
            if let Some(player) = images.video(&key) {
                return Some(player);
            }
            images
                .video_tasks
                .retain(|_, token| !token.load(Ordering::Acquire));
            if images.video_failures.contains_key(&key)
                || images.video_tasks.contains_key(&key)
                || images.video_tasks.len() >= 2
            {
                return None;
            }
            images.video_tasks.insert(key.clone(), cancel.clone());
        }
        let source = file.clone();
        let path = path.to_path_buf();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let worker_cancel = cancel.clone();
            let verified = gio::spawn_blocking(move || {
                let input = crate::downloads::verified_input(&path, &source, &worker_cancel)?;
                // Plugin scanning and Clapper initialization must never block GTK input.
                if unsafe { monogram_playback_init() } == 0 {
                    return Err("Video playback initialization failed");
                }
                Ok(input)
            })
            .await;
            let Some(ui) = weak.upgrade() else {
                return;
            };
            {
                let mut images = ui.images.borrow_mut();
                if images
                    .video_tasks
                    .get(&key)
                    .is_none_or(|token| !Arc::ptr_eq(token, &cancel))
                {
                    return;
                }
                images.video_tasks.remove(&key);
            }
            // A network reconnect does not invalidate authenticated local bytes.
            if ui.state.borrow().account != owner || cancel.load(Ordering::Acquire) {
                return;
            }
            match verified {
                Ok(Ok(input)) => {
                    if let Some(player) = Playback::new(input) {
                        ui.images.borrow_mut().remember_video(key, player);
                    } else {
                        ui.images
                            .borrow_mut()
                            .video_failures
                            .insert(key, "Video playback unavailable");
                    }
                }
                _ => {
                    ui.images
                        .borrow_mut()
                        .video_failures
                        .insert(key, "Video failed integrity verification");
                }
            }
        });
        None
    }
    pub fn attach_video(
        self: &Rc<Self>,
        message: &Message,
        content: &gtk::Box,
        controls: &gtk::Box,
    ) {
        let file = message.attachment.as_ref().unwrap().clone();
        let owner = self.state.borrow().account.clone();
        let peer = self.state.borrow().current().jid.clone();
        let id = message.id.clone();
        let image = AspectImage::thumbnail(320, false);
        let button = gtk::Button::builder()
            .child(&image)
            .visible(false)
            .halign(gtk::Align::Start)
            .tooltip_text("Play video")
            .build();
        button.set_widget_name(&id);
        button.add_css_class("shared-media-tile");
        button.add_css_class("message-media-tile");
        let weak = Rc::downgrade(self);
        if !file.sticker {
            button.connect_clicked(move |button| {
                if let Some(ui) = weak.upgrade() {
                    ui.media_viewer(Some(button.clone().upcast()));
                }
            });
        }
        content.append(&button);
        let weak = Rc::downgrade(self);
        let view = image.downgrade();
        let tile = button.downgrade();
        let fallback = controls.downgrade();
        let location = content.downgrade();
        let mut attached: Option<InlinePlayback> = None;
        let load = move || {
            let (Some(ui), Some(image), Some(button), Some(controls), Some(location)) = (
                weak.upgrade(),
                view.upgrade(),
                tile.upgrade(),
                fallback.upgrade(),
                location.upgrade(),
            ) else {
                return glib::ControlFlow::Break;
            };
            if !ui.attachment_is_current(&owner, &peer, &id, &file) {
                return glib::ControlFlow::Break;
            }
            if let Some(binding) = attached.as_ref()
                && binding.player.media.error().is_some()
            {
                controls.add_css_class("media-preview-error");
                controls.set_tooltip_text(Some("Video playback unavailable"));
                controls.set_visible(true);
                button.set_visible(false);
                attached.take();
                return glib::ControlFlow::Continue;
            }
            if let Some(binding) = attached.as_ref() {
                let player = &binding.player;
                if player.media.is_prepared() {
                    if !player.media.has_video() {
                        controls.add_css_class("media-preview-error");
                        controls.set_tooltip_text(Some("This attachment has no video track"));
                        controls.set_visible(true);
                        return glib::ControlFlow::Continue;
                    }
                    if !button.is_visible() {
                        image.set_paintable(Some(player.media.clone().upcast()));
                        button.set_visible(true);
                        reveal_media(&controls, &player.media);
                        let width = player.media.intrinsic_width().max(0) as u32;
                        let height = player.media.intrinsic_height().max(0) as u32;
                        if width > 0 && height > 0 {
                            let mime = if file.mime.starts_with("video/") {
                                file.mime.as_str()
                            } else {
                                "video/unknown"
                            };
                            ui.remember_media_metadata(
                                &owner, &peer, &id, &file, mime, width, height, true,
                            );
                        }
                    }
                    binding.visible(ui.in_chat_viewport(&location));
                }
                return glib::ControlFlow::Continue;
            }
            if !ui.in_chat_prefetch(&location) {
                return glib::ControlFlow::Continue;
            }
            if let Some(reason) = ui.images.borrow().video_failures.get(&file.key()) {
                controls.add_css_class("media-preview-error");
                controls.set_tooltip_text(Some(reason));
                controls.set_visible(true);
            }
            if let Some(path) = ui.cached_image_source(&file)
                && let Some(player) = ui.video(&file, &path)
            {
                attached = Some(InlinePlayback::new(player, &image));
            }
            glib::ControlFlow::Continue
        };
        let mut load = load;
        if load() == glib::ControlFlow::Continue {
            glib::timeout_add_local(Duration::from_millis(150), load);
        }
    }
}
