//! A message that is exactly one bundled emoji plays its Noto animation once when it scrolls into
//! view and again when clicked. It rests on the first frame, which looks like the static emoji.
//!
//! Frames come from the confined renderer (`lottie::Decoder::open_emoji`) on worker threads, so
//! nothing is parsed or drawn by Lottie code in the GTK process. The widget has a fixed size from
//! the start, so the bubble never reflows when the first frame arrives. With GNOME's animations
//! turned off the animation is never decoded and the ordinary glyph stays.
use crate::widgets::*;
use gtk::{gdk, gio, glib, prelude::*};
use std::{cell::{Cell, RefCell}, rc::Rc, sync::Arc, time::Duration};

/// Logical size of the animation; the renderer draws at twice this for sharpness on HiDPI.
const SIZE: i32 = 96;
const RENDER_EDGE: u32 = 192;

struct State {
    bytes: Arc<Vec<u8>>,
    picture: gtk::Picture,
    glyph: gtk::Widget,
    playing: Cell<bool>,
    /// First frame, shown again once an animation finishes.
    rest: RefCell<Option<gdk::Texture>>,
}

/// The widget for one emoji message. `glyph` is the emoji as text (the fallback and accessible
/// name) and `animation` its gzip Lottie data from `noto_emoji`.
pub fn widget(glyph: &str, animation: Vec<u8>) -> gtk::Widget {
    let label = label(glyph, "large-emoji");
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    let picture = gtk::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Contain)
        .visible(false)
        .build();
    let root = gtk::Overlay::builder().child(&label).build();
    root.add_overlay(&picture);
    root.set_size_request(SIZE, SIZE);
    root.set_halign(gtk::Align::Start);
    root.add_css_class("animated-emoji");
    root.update_property(&[gtk::accessible::Property::Label(&format!("{glyph}, animated emoji"))]);

    let animations = gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations());
    if !animations {
        return root.upcast();
    }
    let state = Rc::new(State {
        bytes: Arc::new(animation),
        picture,
        glyph: label.upcast(),
        playing: Cell::new(false),
        rest: RefCell::new(None),
    });
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    let weak = Rc::downgrade(&state);
    click.connect_pressed(move |_, presses, _, _| {
        // A double click is the message reply gesture; only a single click replays.
        if presses == 1 {
            if let Some(state) = weak.upgrade() {
                play(&state);
            }
        }
    });
    root.add_controller(click);
    wait_until_visible(Rc::downgrade(&state), root.downgrade());
    // The state lives as long as the widget does.
    unsafe { root.set_data("animated-emoji", state) };
    root.upcast()
}

fn visible(root: &gtk::Widget) -> bool {
    if !root.is_mapped() {
        return false;
    }
    let Some(scroll) = root
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_then(|widget| widget.downcast::<gtk::ScrolledWindow>().ok())
    else {
        return true;
    };
    root.compute_bounds(&scroll)
        .is_some_and(|bounds| bounds.y() + bounds.height() >= 0. && bounds.y() <= scroll.height() as f32)
}

/// Play once when the message first comes into view.
fn wait_until_visible(state: std::rc::Weak<State>, root: glib::WeakRef<gtk::Overlay>) {
    glib::timeout_add_local_once(Duration::from_millis(250), move || {
        let (Some(state_ref), Some(root_ref)) = (state.upgrade(), root.upgrade()) else {
            return;
        };
        if visible(root_ref.upcast_ref()) {
            play(&state_ref);
        } else {
            wait_until_visible(state, root);
        }
    });
}

fn play(state: &Rc<State>) {
    if state.playing.replace(true) {
        return;
    }
    let state = state.clone();
    glib::spawn_future_local(async move {
        let permit = crate::media_library_ui::lottie_permit().await;
        let bytes = state.bytes.clone();
        let opened = gio::spawn_blocking(move || {
            let source = crate::lottie::memory_source(&bytes)?;
            let mut decoder = crate::lottie::Decoder::open_emoji(source, RENDER_EDGE, &gio::Cancellable::new())?;
            let first = decoder.frame_at(0)?;
            Ok::<_, &'static str>((decoder, first))
        })
        .await;
        drop(permit);
        let Ok(Ok((decoder, first))) = opened else {
            state.playing.set(false);
            return;
        };
        if state.picture.root().is_none() {
            state.playing.set(false);
            return;
        }
        state.rest.replace(Some(first.texture.clone()));
        show(&state, &first.texture);
        step(state, decoder, std::time::Instant::now(), first.delay);
    });
}

fn show(state: &State, texture: &gdk::Texture) {
    state.picture.set_paintable(Some(texture));
    state.picture.set_visible(true);
    state.glyph.set_opacity(0.);
}

/// Pull the frame the clock says is due after `delay`, stopping when the animation ends or the
/// row is gone or off screen. A slow frame makes the next one skip ahead; frames never queue up.
fn step(state: Rc<State>, decoder: crate::lottie::Decoder, started: std::time::Instant, delay: Duration) {
    glib::timeout_add_local_once(delay.max(Duration::from_millis(1)), move || {
        let index = (started.elapsed().as_secs_f64() * decoder.fps) as u32;
        let finished = index >= decoder.frames || state.picture.root().is_none() || !state.picture.is_mapped();
        if finished {
            if let Some(rest) = state.rest.borrow().as_ref() {
                state.picture.set_paintable(Some(rest));
            }
            state.playing.set(false);
            // Dropping the decoder ends the sandbox process; do not wait for it on the GTK thread.
            gio::spawn_blocking(move || drop(decoder));
            return;
        }
        glib::spawn_future_local(async move {
            let permit = crate::media_library_ui::lottie_permit().await;
            let pulled = gio::spawn_blocking(move || {
                let mut decoder = decoder;
                let frame = decoder.frame_at(index);
                (decoder, frame)
            })
            .await;
            drop(permit);
            let Ok((decoder, Ok(frame))) = pulled else {
                state.playing.set(false);
                return;
            };
            state.picture.set_paintable(Some(&frame.texture));
            // The next frame is due at the next tick of the animation's own clock.
            let elapsed = started.elapsed();
            let tick = ((elapsed.as_secs_f64() * decoder.fps).floor() + 1.) / decoder.fps;
            step(state, decoder, started, Duration::from_secs_f64(tick).saturating_sub(elapsed));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture_of(root: &gtk::Widget) -> gtk::Picture {
        root.last_child().and_downcast::<gtk::Picture>().expect("the picture is the overlay child")
    }

    /// Iterate the main context until `done` or the timeout.
    fn wait(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
        let context = glib::MainContext::default();
        let until = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < until {
            while context.iteration(false) {}
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[gtk::test]
    #[ignore = "requires a native display, the built sandboxed renderer and bubblewrap"]
    fn a_bundled_emoji_swaps_in_moving_frames_and_rests_on_the_first_frame() {
        let animation = crate::noto_emoji::animation_for("😄").expect("😄 is bundled");
        let emoji = widget("😄", animation);
        assert_eq!((emoji.width_request(), emoji.height_request()), (SIZE, SIZE), "fixed size: no reflow");
        let picture = picture_of(&emoji);
        assert!(!picture.is_visible(), "the static glyph shows until a frame arrives");
        let window = gtk::Window::builder().default_width(300).default_height(300).child(&emoji).build();
        window.present();

        let changes = Rc::new(Cell::new(0u32));
        let (first, last) = (Rc::new(Cell::new(None::<std::time::Instant>)), Rc::new(Cell::new(None::<std::time::Instant>)));
        {
            let (changes, first, last) = (changes.clone(), first.clone(), last.clone());
            picture.connect_paintable_notify(move |_| {
                changes.set(changes.get() + 1);
                let now = std::time::Instant::now();
                first.set(first.get().or(Some(now)));
                last.set(Some(now));
            });
        }
        assert!(wait(Duration::from_secs(10), || picture.is_visible()), "first frame arrives");
        // 😄 is 140 frames at 60 fps (2.3 s). Wait for it to finish and settle.
        wait(Duration::from_secs(5), || false);
        let shown = changes.get();
        let played = last.get().unwrap().duration_since(first.get().unwrap());
        eprintln!("{shown} frames shown over {played:?}");
        assert!(shown >= 30, "only {shown} frames were shown");
        assert!(played > Duration::from_millis(1800) && played < Duration::from_millis(3500), "ran for {played:?}, expected about 2.3 s");
        // It rests on the first frame, and a click plays it again.
        let before = changes.get();
        click(&emoji);
        assert!(wait(Duration::from_secs(3), || changes.get() > before + 10), "a click replays the animation");
        window.close();
    }

    fn click(widget: &gtk::Widget) {
        let controllers = widget.observe_controllers();
        for index in 0..controllers.n_items() {
            if let Some(gesture) = controllers.item(index).and_downcast::<gtk::GestureClick>() {
                gesture.emit_by_name::<()>("pressed", &[&1i32, &1.0f64, &1.0f64]);
            }
        }
    }
}
