//! A message that is exactly one bundled emoji plays its Noto animation once when it scrolls into
//! view and again when clicked, and holds its marked resting pose when it ends. The ordinary emoji only
//! shows until the first frame arrives and wherever the animation cannot play.
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

/// At most this many animations play at once. Each one is a confined helper process, so a flood
/// of single-emoji messages scrolling past cannot start a crowd of them: the rest wait their turn.
const MAX_PLAYING: u32 = 4;

thread_local! {
    static ACTIVE: Cell<u32> = const { Cell::new(0) };
}
/// One of the `MAX_PLAYING` places; given back when dropped.
struct Slot;
impl Slot {
    fn take() -> Option<Slot> {
        ACTIVE.with(|active| {
            (active.get() < MAX_PLAYING).then(|| {
                active.set(active.get() + 1);
                Slot
            })
        })
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(active.get().saturating_sub(1)));
    }
}

struct State {
    slot: RefCell<Option<Slot>>,
    bytes: Arc<Vec<u8>>,
    picture: gtk::Picture,
    glyph: gtk::Widget,
    playing: Cell<bool>,
}

/// A symbol such as ❤ or ☺ is drawn as a flat text glyph unless it carries the emoji variation
/// selector. This message is an emoji with a colour animation, so its ordinary form is the colour
/// emoji too; otherwise it would change style when the animation ends.
fn emoji_presentation(glyph: &str) -> String {
    let mut chars = glyph.chars();
    match (chars.next(), chars.next()) {
        (Some(only), None) if (only as u32) < 0x1F000 => format!("{only}\u{fe0f}"),
        _ => glyph.to_owned(),
    }
}

/// The widget for one emoji message. `glyph` is the emoji as text (the fallback and accessible
/// name) and `animation` its gzip Lottie data from `noto_emoji`.
pub fn widget(glyph: &str, animation: Vec<u8>) -> gtk::Widget {
    let label = label(&emoji_presentation(glyph), "single-emoji");
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
        slot: RefCell::new(None),
        bytes: Arc::new(animation),
        picture,
        glyph: label.upcast(),
        playing: Cell::new(false),
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
        // Not visible yet, or every animation place is taken: look again shortly.
        if visible(root_ref.upcast_ref()) && play(&state_ref) {
            return;
        }
        wait_until_visible(state, root);
    });
}

/// Start playing; false when it is already playing or all places are taken.
fn play(state: &Rc<State>) -> bool {
    if state.playing.get() {
        return false;
    }
    let Some(slot) = Slot::take() else { return false };
    state.slot.replace(Some(slot));
    state.playing.set(true);
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
        let (decoder, first) = match opened {
            Ok(Ok(started)) => started,
            failed => {
                // The ordinary emoji stays; say why, so a stale or missing renderer is findable.
                eprintln!(
                    "Monogram animated emoji unavailable: {}",
                    failed.err().map_or("renderer failed", |_| "worker stopped")
                );
                stop(&state);
                return;
            }
        };
        if state.picture.root().is_none() {
            stop(&state);
            return;
        }
        show(&state, &first.texture);
        step(state, decoder, std::time::Instant::now(), first.delay);
    });
    true
}

/// Playback is over (finished, failed, stopped early or the row went away): free the place. The
/// picture on screen stays as it is; if no frame was ever shown, the ordinary emoji is still there.
fn stop(state: &State) {
    state.playing.set(false);
    state.slot.take();
}

fn show_fallback(state: &State) {
    state.picture.set_visible(false);
    state.glyph.set_opacity(1.);
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
        let ended = index >= decoder.frames;
        let gone = state.picture.root().is_none() || !state.picture.is_mapped();
        if ended && !gone {
            let Some(rest) = decoder.rest_frame else {
                // Unmarked animations have no reliable pose to hold after their outro.
                show_fallback(&state);
                stop(&state);
                gio::spawn_blocking(move || drop(decoder));
                return;
            };
            glib::spawn_future_local(async move {
                let permit = crate::media_library_ui::lottie_permit().await;
                let pulled = gio::spawn_blocking(move || {
                    let mut decoder = decoder;
                    let frame = decoder.frame_at(rest);
                    (decoder, frame)
                })
                .await;
                drop(permit);
                if let Ok((decoder, frame)) = pulled {
                    if let Ok(frame) = frame {
                        state.picture.set_paintable(Some(&frame.texture));
                    } else {
                        show_fallback(&state);
                    }
                    // Dropping the decoder ends the sandbox process; not on the GTK thread.
                    gio::spawn_blocking(move || drop(decoder));
                } else {
                    show_fallback(&state);
                }
                stop(&state);
            });
            return;
        }
        if ended || gone {
            stop(&state);
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
                stop(&state);
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

    #[test]
    fn symbols_get_the_emoji_selector_and_real_emoji_are_left_alone() {
        assert_eq!(emoji_presentation("\u{2764}"), "\u{2764}\u{fe0f}", "a text-style heart becomes the colour emoji");
        assert_eq!(emoji_presentation("\u{2764}\u{fe0f}"), "\u{2764}\u{fe0f}", "already has it");
        assert_eq!(emoji_presentation("😄"), "😄");
        assert_eq!(emoji_presentation("👍🏽"), "👍🏽", "a sequence is left as it is");
    }

    #[test]
    fn only_a_few_animations_play_at_once_and_places_are_given_back() {
        let held: Vec<Slot> = (0..MAX_PLAYING).map(|_| Slot::take().expect("a free place")).collect();
        assert!(Slot::take().is_none(), "a flood of emoji messages cannot start more helper processes");
        let mut held = held;
        drop(held.pop());
        let again = Slot::take();
        assert!(again.is_some(), "a finished animation frees its place");
        assert!(Slot::take().is_none());
        drop(held);
        drop(again);
        assert!(Slot::take().is_some(), "everything was given back, and the count never goes below zero");
    }

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

    /// The ordinary emoji and its animation must be the same size, or the emoji visibly shrinks
    /// when the animation ends. Measured, not assumed: the animation's opaque area over several
    /// frames against the glyph's ink at the real stylesheet size.
    #[gtk::test]
    #[ignore = "requires a native display and the built sandboxed renderer"]
    fn the_ordinary_emoji_is_drawn_at_the_size_of_its_animation() {
        let css = gtk::CssProvider::new();
        css.load_from_path(crate::widgets::asset("style.css"));
        gtk::style_context_add_provider_for_display(&gtk::gdk::Display::default().unwrap(), &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let pack = crate::noto_emoji::curated().unwrap();
        let cancel = gio::Cancellable::new();
        let edge = 192u32;
        // "❤" without the variation selector is included: it must still be drawn as the colour emoji.
        for (glyph, key) in [("😄", "1f604"), ("👍", "1f44d"), ("🔥", "1f525"), ("😂", "1f602"), ("🎉", "1f389"), ("❤", "2764"), ("❤\u{fe0f}", "2764")] {
            // Largest extent of the animated emoji across a few frames, in logical pixels.
            let source = crate::lottie::memory_source(pack.get(key).unwrap()).unwrap();
            let mut decoder = crate::lottie::Decoder::open_emoji(source, edge, &cancel).unwrap();
            let (mut x0, mut y0, mut x1, mut y1) = (edge, edge, 0, 0);
            for index in [0, decoder.frames / 4, decoder.frames / 2, decoder.frames * 3 / 4] {
                let texture = decoder.frame_at(index).unwrap().texture;
                let mut px = vec![0u8; (edge * edge * 4) as usize];
                texture.download(&mut px, (edge * 4) as usize);
                for y in 0..edge {
                    for x in 0..edge {
                        if px[((y * edge + x) * 4 + 3) as usize] > 24 {
                            (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
                        }
                    }
                }
            }
            let animated = f64::from((x1 - x0 + 1).max(y1 - y0 + 1)) / f64::from(edge) * f64::from(SIZE);

            let emoji = widget(glyph, crate::noto_emoji::animation_for(glyph).unwrap());
            let label = emoji.first_child().and_downcast::<gtk::Label>().expect("the glyph label");
            let window = gtk::Window::builder().child(&emoji).build();
            window.present();
            wait(Duration::from_millis(200), || false);
            let (ink, _) = label.layout().pixel_extents();
            let drawn = f64::from(ink.width().max(ink.height()));
            let (_, wants_w, _, _) = label.measure(gtk::Orientation::Horizontal, -1);
            let (_, wants_h, _, _) = label.measure(gtk::Orientation::Vertical, -1);
            eprintln!("{glyph}: animation {animated:.0} px, ordinary emoji {drawn:.0} px, glyph box {wants_w}x{wants_h} in {SIZE}");
            assert!((animated - drawn).abs() <= 0.10 * f64::from(SIZE), "{glyph}: animation {animated:.0} px but ordinary emoji {drawn:.0} px");
            assert!(wants_w <= SIZE && wants_h <= SIZE, "{glyph}: the glyph must fit the {SIZE} px box so the widget never grows");
            window.close();
        }
    }

    #[gtk::test]
    #[ignore = "requires a native display, the built sandboxed renderer and bubblewrap"]
    fn a_bundled_emoji_plays_and_holds_its_marked_rest_pose() {
        let animation = crate::noto_emoji::animation_for("🤑").expect("🤑 is bundled");
        let emoji = widget("🤑", animation);
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
        // 🤑 is 143 frames at60fps; its rest pose is110, before the outro.
        wait(Duration::from_secs(5), || false);
        let shown = changes.get();
        let played = last.get().unwrap().duration_since(first.get().unwrap());
        eprintln!("{shown} frames shown over {played:?}");
        assert!(shown >= 30, "only {shown} frames were shown");
        assert!(played > Duration::from_millis(1800) && played < Duration::from_millis(3500), "ran for {played:?}, expected about 2.3 s");
        // Keep the marked static pose, not a frame from the outro.
        let glyph = emoji.first_child().expect("the emoji glyph");
        assert!(picture.is_visible() && glyph.opacity() == 0., "the animation stays, the ordinary emoji does not come back");
        let shown = picture.paintable().and_downcast::<gdk::Texture>().expect("a frame is kept");
        let source = crate::lottie::memory_source(&crate::noto_emoji::animation_for("🤑").unwrap()).unwrap();
        let mut reference = crate::lottie::Decoder::open_emoji(source, RENDER_EDGE, &gio::Cancellable::new()).unwrap();
        let rest = reference.rest_frame.expect("marked static pose");
        assert_eq!(rest,110);
        assert_eq!(shown.save_to_png_bytes(), reference.frame_at(rest).unwrap().texture.save_to_png_bytes(), "holds the marked static pose");
        assert_ne!(shown.save_to_png_bytes(), reference.frame_at(reference.frames-1).unwrap().texture.save_to_png_bytes(), "must not hold the outro");
        // A click plays it again and it freezes again.
        let before = changes.get();
        click(&emoji);
        assert!(wait(Duration::from_secs(3), || changes.get() > before + 10), "a click replays the animation");
        assert!(wait(Duration::from_secs(5), || ACTIVE.with(Cell::get) == 0), "and it finishes");
        assert!(picture.is_visible() && glyph.opacity() == 0.);
        window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display, the built sandboxed renderer and bubblewrap"]
    fn an_unmarked_animation_returns_to_the_ordinary_emoji() {
        let emoji = widget("👀", crate::noto_emoji::animation_for("👀").unwrap());
        let picture = picture_of(&emoji);
        let glyph = emoji.first_child().unwrap();
        let window = gtk::Window::builder().default_width(300).default_height(300).child(&emoji).build();
        window.present();
        assert!(wait(Duration::from_secs(10), || picture.is_visible()));
        assert!(wait(Duration::from_secs(5), || ACTIVE.with(Cell::get) == 0));
        assert!(!picture.is_visible() && glyph.opacity() == 1.);
        click(&emoji);
        assert!(wait(Duration::from_secs(3), || picture.is_visible()), "fallback must not prevent replay");
        assert!(wait(Duration::from_secs(5), || ACTIVE.with(Cell::get) == 0));
        assert!(!picture.is_visible() && glyph.opacity() == 1.);
        window.close();
    }

    /// Twenty emoji messages on screen at once: never more than the cap play together, yet they
    /// do start, and the places are all given back afterwards.
    #[gtk::test]
    #[ignore = "requires a native display, the built sandboxed renderer and bubblewrap"]
    fn a_flood_of_visible_emoji_never_plays_more_than_the_cap_at_once() {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for _ in 0..20 {
            column.append(&widget("😄", crate::noto_emoji::animation_for("😄").unwrap()));
        }
        let scroll = gtk::ScrolledWindow::builder().child(&column).build();
        let window = gtk::Window::builder().default_width(300).default_height(2200).child(&scroll).build();
        window.present();
        let (mut peak, mut started) = (0, false);
        wait(Duration::from_secs(5), || {
            let active = ACTIVE.with(Cell::get);
            peak = peak.max(active);
            started |= active > 0;
            false
        });
        assert!(started, "animations did start");
        assert!(peak <= MAX_PLAYING, "{peak} played at once, the cap is {MAX_PLAYING}");
        window.close();
        wait(Duration::from_secs(2), || false);
    }

    fn has_class(widget: &gtk::Widget, class: &str) -> bool {
        widget.has_css_class(class) || {
            let mut child = widget.first_child();
            let mut found = false;
            while let Some(current) = child {
                found |= has_class(&current, class);
                child = current.next_sibling();
            }
            found
        }
    }

    /// The path the app really takes: send from the composer, render the timeline, look at the
    /// bubble. A widget that works alone proves nothing if messages never reach it.
    #[gtk::test]
    #[ignore = "requires a native display, the built sandboxed renderer and bubblewrap"]
    fn only_messages_that_are_exactly_one_bundled_emoji_get_the_animated_widget() {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.negzero.AnimatedEmojiMessageFixture")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = crate::ui::Ui::new(&app, true);
        ui.window.present();
        let unbundled = "🪿";
        assert!(crate::noto_emoji::animation_for(unbundled).is_none(), "fixture must be outside the pack");
        let texts = ["😄", "😄😄", "hi 😄", unbundled, "👍"];
        for text in texts {
            ui.composer.buffer().set_text(text);
            ui.send();
            wait(Duration::from_millis(300), || false);
        }
        wait(Duration::from_millis(1200), || false);
        let result = {
            let (state, bubbles) = (ui.state.borrow(), ui.bubbles.borrow());
            state
                .current()
                .messages
                .iter()
                .filter(|m| texts.contains(&m.text.as_str()))
                .map(|m| {
                    let bubble = bubbles.get(&m.id).unwrap_or_else(|| panic!("{:?} was rendered", m.text));
                    (m.text.clone(), has_class(bubble, "animated-emoji"))
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            result,
            [
                ("😄".to_string(), true),
                ("😄😄".to_string(), false),
                ("hi 😄".to_string(), false),
                (unbundled.to_string(), false),
                ("👍".to_string(), true),
            ]
        );
        // Every message that is exactly one emoji is drawn at the animation's size, bundled or
        // not; two emoji keep the smaller size.
        let sizes = {
            let (state, bubbles) = (ui.state.borrow(), ui.bubbles.borrow());
            ["😄", unbundled, "😄😄"].map(|text| {
                let id = &state.current().messages.iter().find(|m| m.text == text).unwrap().id;
                let bubble = bubbles.get(id).unwrap();
                (has_class(bubble, "single-emoji"), has_class(bubble, "large-emoji"))
            })
        };
        assert_eq!(sizes, [(true, false), (true, false), (false, true)]);
        ui.window.close();
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
