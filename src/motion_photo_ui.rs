//! Still-first, explicitly requested one-shot playback, shared by chat and the viewer.
use crate::{file_share::Attachment, ui::Ui, video_ui::Playback};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::{Rc, Weak},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub(crate) struct MotionPhoto {
    ui: Weak<Ui>,
    owner: String,
    generation: u64,
    file: Attachment,
    path: PathBuf,
    target: glib::WeakRef<gtk::Widget>,
    badge: glib::WeakRef<gtk::Button>,
    still: gdk::Paintable,
    draw: Box<dyn Fn(&gdk::Paintable)>,
    active: Box<dyn Fn() -> bool>,
    player: RefCell<Option<Rc<Playback>>>,
    pending: Cell<bool>,
    playing: Cell<bool>,
    cancel: Arc<AtomicBool>,
    epoch: Cell<u64>,
}
impl MotionPhoto {
    pub(crate) fn new(
        ui: &Rc<Ui>,
        file: &Attachment,
        path: &std::path::Path,
        target: &impl IsA<gtk::Widget>,
        badge: &gtk::Button,
        still: gdk::Paintable,
        draw: impl Fn(&gdk::Paintable) + 'static,
        active: impl Fn() -> bool + 'static,
    ) -> Rc<Self> {
        badge.set_tooltip_text(Some("Play motion photo"));
        badge.update_property(&[gtk::accessible::Property::Label("Play motion photo")]);
        let this = Rc::new(Self {
            ui: Rc::downgrade(ui),
            owner: ui.state.borrow().account.clone(),
            generation: ui.generation.get(),
            file: file.clone(),
            path: path.to_path_buf(),
            target: target.as_ref().downgrade(),
            badge: badge.downgrade(),
            still,
            draw: Box::new(draw),
            active: Box::new(active),
            player: RefCell::new(None),
            pending: Cell::new(false),
            playing: Cell::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            epoch: Cell::new(0),
        });
        let weak = Rc::downgrade(&this);
        target.connect_unmap(move |_| {
            if let Some(this) = weak.upgrade() {
                this.cancel.store(true, Ordering::Release);
                this.reset();
            }
        });
        this
    }
    fn valid(&self) -> bool {
        (self.active)()
            && self.target.upgrade().is_some_and(|w| w.is_mapped())
            && self.ui.upgrade().is_some_and(|ui| {
                ui.state.borrow().account == self.owner && ui.generation.get() == self.generation
            })
    }
    fn reset(&self) {
        self.epoch.set(self.epoch.get().wrapping_add(1));
        self.playing.set(false);
        if let Some(player) = self.player.borrow_mut().take() {
            player.media.pause();
        }
        (self.draw)(&self.still);
        if (self.active)()
            && let Some(badge) = self.badge.upgrade()
        {
            badge.set_icon_name("media-playback-start-symbolic");
            badge.set_tooltip_text(Some("Play motion photo"));
            badge.update_property(&[gtk::accessible::Property::Label("Play motion photo")]);
        }
    }
    pub(crate) fn play(self: &Rc<Self>) {
        if !self.valid() || self.pending.get() {
            return;
        }
        if self.playing.get() {
            self.reset();
            return;
        }
        if let Some(player) = self.player.borrow().clone() {
            self.start(player);
            return;
        }
        self.pending.set(true);
        self.cancel.store(false, Ordering::Release);
        if let Some(badge) = self.badge.upgrade() {
            badge.set_child(Some(&adw::Spinner::new()));
            badge.set_tooltip_text(Some("Preparing motion photo…"));
        }
        let file = self.file.clone();
        let path = self.path.clone();
        let cancel = self.cancel.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gio::spawn_blocking(move || {
                let input = crate::downloads::verified_input(&path, &file, &cancel)?;
                let clip = crate::motion_photo::extract(input, &cancel)?;
                if !crate::video_ui::initialize() {
                    return Err("Motion photo playback unavailable");
                }
                Ok(clip)
            })
            .await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            this.pending.set(false);
            if let Some(badge) = this.badge.upgrade() {
                badge.set_icon_name("media-playback-start-symbolic");
            }
            if !this.valid() || this.cancel.load(Ordering::Acquire) {
                return;
            }
            match result {
                Ok(Ok(input)) => {
                    if let Some(player) = Playback::new(input) {
                        this.player.replace(Some(player.clone()));
                        this.start(player);
                    } else {
                        this.failed();
                    }
                }
                _ => this.failed(),
            }
        });
    }
    fn failed(&self) {
        self.reset();
        if let Some(ui) = self.ui.upgrade() {
            ui.notice("Motion photo could not play. Try again or save the original photo.");
        }
    }
    fn start(self: &Rc<Self>, player: Rc<Playback>) {
        player.media.set_loop(false);
        player.media.set_muted(false);
        if player.media.is_prepared() && player.media.is_seekable() {
            player.media.seek(0);
        }
        self.epoch.set(self.epoch.get().wrapping_add(1));
        let epoch = self.epoch.get();
        self.playing.set(true);
        if let Some(badge) = self.badge.upgrade() {
            badge.set_icon_name("media-playback-stop-symbolic");
            badge.set_tooltip_text(Some("Stop motion photo"));
            badge.update_property(&[gtk::accessible::Property::Label("Stop motion photo")]);
        }
        let target = self.target.clone();
        let frame_signal = player.media.connect_invalidate_contents(move |_| {
            if let Some(target) = target.upgrade() {
                target.queue_draw();
            }
        });
        player.media.play();
        let weak = Rc::downgrade(self);
        let mut showed = false;
        let mut frame_signal = Some(frame_signal);
        glib::timeout_add_local(Duration::from_millis(40), move || {
            let Some(this) = weak.upgrade() else {
                player.media.disconnect(frame_signal.take().unwrap());
                return glib::ControlFlow::Break;
            };
            if this.epoch.get() != epoch {
                player.media.disconnect(frame_signal.take().unwrap());
                return glib::ControlFlow::Break;
            }
            if !this.playing.get()
                || !this.valid()
                || player.media.is_ended()
                || player.media.error().is_some()
            {
                player.media.disconnect(frame_signal.take().unwrap());
                this.reset();
                return glib::ControlFlow::Break;
            }
            if player.media.is_prepared() && player.media.has_video() {
                if !showed {
                    (this.draw)(player.media.upcast_ref());
                    showed = true;
                }
            }
            glib::ControlFlow::Continue
        });
    }
}
impl Drop for MotionPhoto {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(player) = self.player.get_mut() {
            player.media.pause();
        }
    }
}

pub(crate) fn button() -> gtk::Button {
    let button = gtk::Button::from_icon_name("media-playback-start-symbolic");
    button.add_css_class("osd");
    button.add_css_class("circular");
    button.set_halign(gtk::Align::Start);
    button.set_valign(gtk::Align::Start);
    button.set_margin_start(8);
    button.set_margin_top(8);
    button.set_visible(false);
    button
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    fn children(root: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
        let mut out = vec![];
        let mut pending = vec![root.as_ref().clone()];
        while let Some(w) = pending.pop() {
            let mut child = w.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                pending.push(c);
            }
            out.push(w);
        }
        out
    }
    async fn wait(mut predicate: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(12);
        while !predicate() {
            assert!(
                std::time::Instant::now() < deadline,
                "motion photo did not settle"
            );
            glib::timeout_future(Duration::from_millis(10)).await;
        }
    }
    #[test]
    #[ignore = "requires private native display, Secret Service and media plugins"]
    fn chat_motion_photo_starts_still_plays_once_returns_still_and_replays() {
        let app = adw::Application::builder()
            .application_id("com.negzero.MotionPhotoTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let ui = Ui::new(&app, true);
        let owner = "owner@fixture.invalid";
        let peer = "friend@fixture.invalid";
        ui.store
            .save_account(&Account {
                jid: owner.into(),
                host: "fixture.invalid".into(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let pixels =
            gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 160, 120)
                .unwrap();
        pixels.fill(0xff8800ff);
        let still = pixels.save_to_bufferv("jpeg", &[]).unwrap();
        let video = include_bytes!("../tests/fixtures/reaction-video.mp4");
        let packet = crate::motion_photo::tests::packet("1", &video.len().to_string());
        let shell = crate::motion_photo::tests::container(&packet, &[]);
        let mut bytes = shell[..shell.len() - 2].to_vec();
        bytes.extend_from_slice(&still[2..]);
        bytes.extend_from_slice(video);
        let mut file = Attachment::bare("https://fixture.invalid/motion.MP.jpg").unwrap();
        file.mime = "image/jpeg".into();
        file.size = Some(bytes.len() as u64);
        file.hashes
            .insert("sha-256".into(), openssl::sha::sha256(&bytes).to_vec());
        let root = crate::downloads::account_root(owner).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(file.key()), &bytes).unwrap();
        // The transport/persistence carries one original JPEG; no separate unprotected URL.
        let mut wire = tokio_xmpp::minidom::Element::builder("message", "jabber:client")
            .append(
                tokio_xmpp::minidom::Element::builder("body", "jabber:client")
                    .append(file.wire_url.as_str())
                    .build(),
            )
            .build();
        file.add_to(&mut wire);
        assert_eq!(
            crate::file_share::Attachment::incoming(&wire, &file.wire_url, false).unwrap(),
            file
        );
        let mut state = State::live(owner, vec![]);
        state.chat_for(peer).messages.push(Message {
            id: "motion".into(),
            wire_id: "motion-wire".into(),
            sender: peer.into(),
            kind: "file".into(),
            attachment: Some(file.clone()),
            timestamp: "2026-11-15T10:00:00Z".into(),
            ..Default::default()
        });
        state.active = state.chat_for(peer).id.clone();
        ui.store.save(&state).unwrap();
        ui.activate_state(state);
        let css = gtk::CssProvider::new();
        css.load_from_path(crate::widgets::asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::prelude::WidgetExt::display(&ui.window),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        ui.downloads
            .borrow_mut()
            .restore_cached(owner, file.clone())
            .unwrap();
        ui.window.set_default_size(1000, 900);
        ui.window.present();
        let last = Rc::new(Cell::new(std::time::Instant::now()));
        let gap = Rc::new(Cell::new(Duration::ZERO));
        let last_tick = last.clone();
        let longest = gap.clone();
        let heartbeat = glib::timeout_add_local(Duration::from_millis(20), move || {
            let now = std::time::Instant::now();
            longest.set(
                longest
                    .get()
                    .max(now.duration_since(last_tick.replace(now))),
            );
            glib::ControlFlow::Continue
        });
        glib::MainContext::default().block_on(async {
            println!("WAIT rendered motion badge");
            wait(|| {
                ui.bubbles.borrow().get("motion").is_some_and(|root| {
                    children(root).iter().any(|w| {
                        w.downcast_ref::<gtk::Button>().is_some_and(|b| {
                            b.tooltip_text().as_deref() == Some("Play motion photo")
                                && b.is_mapped()
                        })
                    })
                })
            })
            .await;
            let root = ui.bubbles.borrow()["motion"].clone();
            let picture = children(&root)
                .into_iter()
                .find_map(|w| w.downcast::<crate::widgets::AspectImage>().ok())
                .unwrap();
            let badge = children(&root)
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Button>().ok())
                .find(|b| b.icon_name().as_deref() == Some("media-playback-start-symbolic"))
                .unwrap();
            let poster = picture.paintable().unwrap();
            assert!(poster.is::<gdk::Texture>());
            glib::timeout_future(Duration::from_millis(150)).await;
            assert_eq!(
                picture.paintable().unwrap(),
                poster,
                "motion photos must not autoplay"
            );
            if std::env::var_os("MONOGRAM_MOTION_SCREENSHOTS").is_some() {
                let _ = std::process::Command::new("import")
                    .args(["-window", "root", "/tmp/monogram-motion-still.png"])
                    .status();
            }
            let start = std::time::Instant::now();
            badge.emit_clicked();
            assert!(badge.child().is_some_and(|c| c.is::<adw::Spinner>()));
            assert!(start.elapsed() < Duration::from_millis(400));
            println!("WAIT first native frame");
            wait(|| {
                picture
                    .paintable()
                    .is_some_and(|p| p.is::<gtk::MediaStream>())
            })
            .await;
            if std::env::var_os("MONOGRAM_MOTION_SCREENSHOTS").is_some() {
                let _ = std::process::Command::new("import")
                    .args(["-window", "root", "/tmp/monogram-motion-playing.png"])
                    .status();
            }
            let stream = picture
                .paintable()
                .unwrap()
                .downcast::<gtk::MediaStream>()
                .unwrap();
            assert!(!stream.is_loop());
            println!(
                "WAIT original poster after EOS duration {}",
                stream.duration()
            );
            wait(|| picture.paintable().as_ref() == Some(&poster)).await;
            assert!(!stream.is_playing());
            badge.emit_clicked();
            wait(|| {
                picture
                    .paintable()
                    .is_some_and(|p| p.is::<gtk::MediaStream>())
            })
            .await;
            badge.emit_clicked();
            assert_eq!(
                picture.paintable().unwrap(),
                poster,
                "stop restores original JPEG, not last video frame"
            );
            badge.emit_clicked();
            wait(|| {
                picture
                    .paintable()
                    .is_some_and(|p| p.is::<gtk::MediaStream>())
            })
            .await;
            ui.window.set_visible(false);
            glib::timeout_future(Duration::from_millis(70)).await;
            assert!(!stream.is_playing());
            ui.window.present();
            let tile = children(&root)
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Button>().ok())
                .find(|b| b.widget_name() == "motion")
                .unwrap();
            ui.media_viewer(Some(tile.upcast()));
            let viewer = ui.active_media.borrow().clone().unwrap();
            println!("WAIT motion photo viewer");
            wait(|| {
                children(&viewer.root).iter().any(|w| {
                    w.downcast_ref::<gtk::Button>().is_some_and(|b| {
                        b.tooltip_text().as_deref() == Some("Play motion photo") && b.is_mapped()
                    })
                })
            })
            .await;
            let view = children(&viewer.root)
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Picture>().ok())
                .find(|p| p.paintable().is_some())
                .unwrap();
            let original = view.paintable().unwrap();
            assert!(original.is::<gdk::Texture>());
            let play = children(&viewer.root)
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Button>().ok())
                .find(|b| b.tooltip_text().as_deref() == Some("Play motion photo"))
                .unwrap();
            play.emit_clicked();
            wait(|| view.paintable().is_some_and(|p| p.is::<gtk::MediaStream>())).await;
            wait(|| view.paintable().as_ref() == Some(&original)).await;
            viewer.close_immediate();
            assert_eq!(
                ui.store
                    .message(owner, peer, "motion")
                    .unwrap()
                    .attachment
                    .unwrap(),
                file
            );
            // A persisted preview must keep the motion metadata after reopening.
            let cache_path = root_path(owner, &file);
            let preview_file = file.clone();
            let image = gio::spawn_blocking(move || {
                crate::image_loader::preview(
                    &preview_file,
                    &cache_path,
                    512,
                    &gio::Cancellable::new(),
                    &AtomicBool::new(false),
                    true,
                )
            })
            .await
            .unwrap()
            .unwrap();
            assert!(image.motion.is_some());
            assert_eq!(
                std::fs::read(root_path(owner, &file)).unwrap(),
                bytes,
                "playback must preserve the original send/save bytes"
            );
        });
        heartbeat.remove();
        println!(
            "Motion photo GTK maximum heartbeat gap: {}ms",
            gap.get().as_millis()
        );
        assert!(gap.get() < Duration::from_millis(400));
    }
    fn root_path(owner: &str, file: &Attachment) -> PathBuf {
        crate::downloads::account_root(owner)
            .unwrap()
            .join(file.key())
    }
}
