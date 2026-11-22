//! The waveform shown while recording: one rounded bar per level reading, newest on the right.
//!
//! It draws with the widget's own CSS `color`, which the stylesheet sets to the accent colour, so
//! the colour scheme, the user's accent and high contrast all apply without any colour code here.
//! The history has a fixed size, and drawing is a handful of rounded rectangles, so it costs
//! almost nothing even on a slow machine.
use super::meter::History;
use gtk::{glib, graphene, gsk, prelude::*, subclass::prelude::*};
use std::cell::RefCell;

/// Bars are this wide, with this much between them, in logical pixels.
pub const BAR_WIDTH: f32 = 3.0;
pub const BAR_GAP: f32 = 2.0;
/// Even silence leaves a dot, so the bar never looks empty or broken.
const MIN_BAR: f32 = 3.0;
/// The most bars ever kept; more than any window is wide enough to show.
const CAPACITY: usize = 512;

/// How many bars fit in `width` pixels.
pub fn bars_that_fit(width: f32) -> usize {
    if width < BAR_WIDTH {
        return 0;
    }
    (((width + BAR_GAP) / (BAR_WIDTH + BAR_GAP)).floor() as usize).min(CAPACITY)
}

/// The height of a bar of level `level` (`0..=1`) in a widget of height `height`.
pub fn bar_pixels(level: f32, height: f32) -> f32 {
    let level = if level.is_finite() { level.clamp(0.0, 1.0) } else { 0.0 };
    (MIN_BAR + level * (height - MIN_BAR).max(0.0)).min(height.max(MIN_BAR))
}

mod imp {
    use super::*;

    pub struct VoiceWave {
        pub history: RefCell<History>,
    }
    impl Default for VoiceWave {
        fn default() -> Self {
            Self { history: RefCell::new(History::new(CAPACITY)) }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VoiceWave {
        const NAME: &'static str = "MonogramVoiceWave";
        type Type = super::VoiceWave;
        type ParentType = gtk::Widget;
        fn class_init(class: &mut Self::Class) {
            // Decorative: the timer next to it carries the information.
            class.set_accessible_role(gtk::AccessibleRole::Presentation);
            class.set_css_name("voicewave");
        }
    }
    impl ObjectImpl for VoiceWave {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.add_css_class("voice-wave");
            obj.set_hexpand(true);
            obj.set_can_target(false);
        }
    }
    impl WidgetImpl for VoiceWave {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (48, 200, -1, -1),
                _ => (24, 32, -1, -1),
            }
        }
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (width, height) = (obj.width() as f32, obj.height() as f32);
            let fit = bars_that_fit(width);
            if fit == 0 || height <= 0.0 {
                return;
            }
            let color = obj.color();
            let history = self.history.borrow();
            let shown = history.recent(fit);
            let count = history.len().min(fit);
            // Newest at the right edge; older bars to its left.
            let mut x = width - BAR_WIDTH;
            let radius = BAR_WIDTH / 2.0;
            let mut bars: Vec<f32> = shown.collect();
            bars.reverse();
            for level in bars.into_iter().take(count) {
                let h = bar_pixels(level, height);
                let rect = graphene::Rect::new(x, (height - h) / 2.0, BAR_WIDTH, h);
                snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, radius));
                snapshot.append_color(&color, &rect);
                snapshot.pop();
                x -= BAR_WIDTH + BAR_GAP;
                if x < -BAR_WIDTH {
                    break;
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct VoiceWave(ObjectSubclass<imp::VoiceWave>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl VoiceWave {
    pub fn new() -> Self {
        glib::Object::new()
    }
    /// Add a reading and redraw. Cheap: nothing is allocated for the history.
    pub fn push(&self, level: f32) {
        self.imp().history.borrow_mut().push(level);
        self.queue_draw();
    }
    pub fn clear(&self) {
        self.imp().history.borrow_mut().clear();
        self.queue_draw();
    }
    pub fn len(&self) -> usize {
        self.imp().history.borrow().len()
    }
}

impl Default for VoiceWave {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_number_of_bars_fits_the_width_and_is_bounded() {
        assert_eq!(bars_that_fit(0.0), 0);
        assert_eq!(bars_that_fit(2.9), 0);
        assert_eq!(bars_that_fit(3.0), 1);
        assert_eq!(bars_that_fit(8.0), 2, "3 + 2 + 3");
        assert_eq!(bars_that_fit(7.9), 1);
        assert_eq!(bars_that_fit(100.0), 20);
        assert_eq!(bars_that_fit(1e9), CAPACITY, "never more than the history holds");
        assert_eq!(bars_that_fit(f32::NAN), 0);
        assert_eq!(bars_that_fit(-5.0), 0);
        // A bar and its gap are 5 px, so every extra 5 px adds one.
        for width in (8..400).step_by(5) {
            assert_eq!(bars_that_fit(width as f32), bars_that_fit(width as f32 - 5.0) + 1, "{width}");
        }
    }

    #[test]
    fn a_bar_is_never_shorter_than_a_dot_nor_taller_than_the_widget() {
        assert_eq!(bar_pixels(0.0, 32.0), MIN_BAR);
        assert_eq!(bar_pixels(1.0, 32.0), 32.0);
        assert!(bar_pixels(0.5, 32.0) > MIN_BAR && bar_pixels(0.5, 32.0) < 32.0);
        assert_eq!(bar_pixels(f32::NAN, 32.0), MIN_BAR);
        assert_eq!(bar_pixels(-3.0, 32.0), MIN_BAR);
        assert_eq!(bar_pixels(9.0, 32.0), 32.0);
        // A widget shorter than the dot still gets a sane bar.
        assert!(bar_pixels(1.0, 2.0) <= MIN_BAR && bar_pixels(1.0, 0.0) <= MIN_BAR);
        let heights: Vec<f32> = (0..=20).map(|n| bar_pixels(n as f32 / 20.0, 40.0)).collect();
        assert!(heights.windows(2).all(|pair| pair[0] <= pair[1]));
    }
}
