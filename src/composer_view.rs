//! Five complete text lines, with native caret scrolling during height changes.
use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;
    #[derive(Default)]
    pub struct ComposerView { height: std::cell::Cell<i32> }
    #[glib::object_subclass]
    impl ObjectSubclass for ComposerView {
        const NAME: &'static str = "MonogramComposerView";
        type Type = super::ComposerView;
        type ParentType = gtk::TextView;
    }
    impl ObjectImpl for ComposerView {}
    impl WidgetImpl for ComposerView {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32,i32,i32,i32) {
            let measured = self.parent_measure(orientation,for_size);
            if orientation != gtk::Orientation::Vertical { return measured; }
            let view = self.obj();
            let mut iter = view.buffer().start_iter();
            let first = view.iter_location(&iter);
            let mut last = first;
            let mut lines = 1;
            // Measuring a large paste only needs the first five display lines.
            for _ in 1..5 {
                let before = iter.offset();
                view.forward_display_line(&mut iter);
                if iter.offset() == before { break; }
                let next = view.iter_location(&iter);
                if next.y() > last.y() { lines += 1; }
                last = next;
                if iter.is_end() { break; }
            }
            let padding = view.top_margin() + view.bottom_margin();
            // One fixed line height, taken from ordinary text. The first line's own height
            // changes with what is on it (a line of emoji, a tall glyph from a fallback font),
            // which made the whole box jump or grow for good.
            let line = view.create_pango_layout(Some("Åg")).pixel_size().1.max(1);
            let natural = padding + lines * line;
            (measured.0.min(natural),natural,-1,-1)
        }
        fn size_allocate(&self, width:i32, height:i32, baseline:i32) {
            // GTK skips updating viewport geometry while a caret scroll is animating.
            // Cancel that old-size scroll before the new allocation clamps it naturally.
            if self.height.replace(height) != height {
                if let Some(adjustment) = self.obj().vadjustment() {
                    adjustment.set_value(adjustment.value());
                }
            }
            self.parent_size_allocate(width,height,baseline);
        }
    }
    impl TextViewImpl for ComposerView {}
}
glib::wrapper! {
    pub struct ComposerView(ObjectSubclass<imp::ComposerView>)
        @extends gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}
impl ComposerView {
    pub(crate) fn new() -> gtk::TextView {
        let view: Self = glib::Object::new();
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_accepts_tab(false);
        view.set_hexpand(true);
        view.set_top_margin(6);
        view.set_bottom_margin(6);
        view.set_left_margin(4);
        view.set_right_margin(4);
        view.upcast()
    }
}
