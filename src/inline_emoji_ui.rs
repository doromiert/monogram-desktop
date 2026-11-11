use crate::{
    packs::{InlineAsset, InlineEmoji, InlineRange, Job, Library},
    ui::Ui,
};
use gtk::{gdk, gio, glib, prelude::*, subclass::prelude::*};
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc, time::Duration};

struct ComposerSpan {
    start: gtk::TextMark,
    end: gtk::TextMark,
    fallback: String,
    asset: InlineAsset,
}

/// Presentation state for custom emoji embedded in the native composer.
/// Text marks keep the Unicode fallback attached to ordinary GTK edits.
pub(crate) struct ComposerInlineEmoji {
    buffer: gtk::TextBuffer,
    spans: RefCell<Vec<ComposerSpan>>,
}

impl ComposerInlineEmoji {
    pub(crate) fn new(buffer: &gtk::TextBuffer) -> Rc<Self> {
        Rc::new(Self {
            buffer: buffer.clone(),
            spans: RefCell::new(Vec::new()),
        })
    }

    pub(crate) fn insert(&self, asset: InlineAsset, fallback: &str) -> bool {
        let body = self
            .buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), true)
            .to_string();
        self.prune_invalid(&body);
        let fallback_chars = fallback.chars().count();
        if fallback_chars == 0
            || fallback_chars > 64
            || fallback.chars().any(char::is_control)
            || body.chars().count().saturating_add(fallback_chars) > 65_536
            || body
                .encode_utf16()
                .count()
                .saturating_add(fallback.encode_utf16().count())
                > 131_072
        {
            return false;
        }
        let spans = self.spans.borrow();
        if spans.len() >= 64 {
            return false;
        }
        let mut assets = HashMap::<&str, (&str, usize)>::new();
        for span in spans.iter() {
            assets
                .entry(&span.asset.cid)
                .or_insert((&span.asset.mime, span.asset.bytes.len()));
        }
        if let Some((mime, _)) = assets.get(asset.cid.as_str()) {
            if *mime != asset.mime {
                return false;
            }
        } else {
            assets.insert(&asset.cid, (&asset.mime, asset.bytes.len()));
        }
        if assets.len() > 16 || assets.values().map(|(_, size)| size).sum::<usize>() > 128 * 1024 {
            return false;
        }
        drop(spans);

        let mut cursor = self.buffer.iter_at_offset(self.buffer.cursor_position());
        let start_offset = cursor.offset();
        self.buffer.insert(&mut cursor, fallback);
        let start = self
            .buffer
            .create_mark(None, &self.buffer.iter_at_offset(start_offset), false);
        let end = self.buffer.create_mark(None, &cursor, true);
        self.spans.borrow_mut().push(ComposerSpan {
            start,
            end,
            fallback: fallback.to_owned(),
            asset,
        });
        true
    }

    pub(crate) fn value(&self) -> Option<InlineEmoji> {
        let body = self
            .buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), true)
            .to_string();
        self.prune_invalid(&body);
        let ranges = self
            .spans
            .borrow()
            .iter()
            .filter_map(|span| {
                let start = self.buffer.iter_at_mark(&span.start).offset().max(0) as usize;
                let end = self.buffer.iter_at_mark(&span.end).offset().max(0) as usize;
                let actual: String = body
                    .chars()
                    .skip(start)
                    .take(end.saturating_sub(start))
                    .collect();
                (end > start && actual == span.fallback).then(|| InlineRange {
                    start,
                    end,
                    asset: span.asset.clone(),
                })
            })
            .collect::<Vec<_>>();
        (!ranges.is_empty())
            .then(|| InlineEmoji::new(body, ranges).ok())
            .flatten()
    }

    fn prune_invalid(&self, body: &str) {
        let chars = body.chars().collect::<Vec<_>>();
        let spans = std::mem::take(&mut *self.spans.borrow_mut());
        let mut valid = Vec::with_capacity(spans.len());
        for span in spans {
            let start = self.buffer.iter_at_mark(&span.start).offset().max(0) as usize;
            let end = self.buffer.iter_at_mark(&span.end).offset().max(0) as usize;
            let matches = end > start
                && end <= chars.len()
                && chars[start..end].iter().collect::<String>() == span.fallback;
            if matches {
                valid.push(span);
            } else {
                self.buffer.delete_mark(&span.start);
                self.buffer.delete_mark(&span.end);
            }
        }
        *self.spans.borrow_mut() = valid;
    }

    pub(crate) fn clear(&self) {
        for span in self.spans.borrow_mut().drain(..) {
            self.buffer.delete_mark(&span.start);
            self.buffer.delete_mark(&span.end);
        }
    }
}

#[cfg(test)]
mod composer_tests {
    use super::*;

    #[test]
    fn ordinary_typing_preserves_span_and_inside_edit_prunes_it() {
        gtk::init().unwrap();
        let buffer = gtk::TextBuffer::new(None);
        let composer = ComposerInlineEmoji::new(&buffer);
        let asset = InlineAsset::new("image/png".into(), vec![1, 2, 3]).unwrap();
        assert!(composer.insert(asset, "🙂"));

        buffer.place_cursor(&buffer.start_iter());
        buffer.insert_at_cursor("before ");
        buffer.place_cursor(&buffer.end_iter());
        buffer.insert_at_cursor(" after");
        let value = composer
            .value()
            .expect("ordinary surrounding text keeps the span");
        assert_eq!(value.ranges.len(), 1);
        assert_eq!(value.body, "before 🙂 after");

        let mut emoji_start = buffer.iter_at_offset(7);
        let mut emoji_end = buffer.iter_at_offset(8);
        buffer.delete(&mut emoji_start, &mut emoji_end);
        assert!(composer.value().is_none());
        assert!(composer.spans.borrow().is_empty());
    }

    #[test]
    #[ignore]
    fn inline_flow_visual_probe() {
        gtk::init().unwrap();
        let fallback = "🙂";
        let asset = InlineAsset::new("image/png".into(), vec![1, 2, 3]).unwrap();
        let short_body = format!("Hello {fallback} world");
        let short_emoji = InlineEmoji::new(
            short_body.clone(),
            vec![InlineRange {
                start: 6,
                end: 7,
                asset: asset.clone(),
            }],
        )
        .unwrap();
        let body = format!(
            "A long prefix that should wrap naturally before the custom emoji {fallback} and continue on the same flowing paragraph."
        );
        let start = body.find(fallback).unwrap();
        let start = body[..start].chars().count();
        let emoji = InlineEmoji::new(
            body.clone(),
            vec![InlineRange {
                start,
                end: start + 1,
                asset,
            }],
        )
        .unwrap();
        let status = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        status.append(&gtk::Label::new(Some("12:34 ✓")));
        let short = InlineMessageText::new(&short_body, &short_emoji, Some(&status));
        let text = InlineMessageText::new(&body, &emoji, None);
        let bytes = glib::Bytes::from_owned(vec![0xff; 20 * 20 * 4]);
        let texture = gdk::MemoryTexture::new(
            20,
            20,
            gdk::MemoryFormat::R8g8b8a8Premultiplied,
            &bytes,
            20 * 4,
        );
        short.set_texture(&short_emoji.ranges[0].asset.cid, texture.clone().upcast());
        text.set_texture(&emoji.ranges[0].asset.cid, texture.upcast());
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.append(&short.root);
        content.append(&text.root);
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        content.set_margin_start(8);
        content.set_margin_end(8);
        text.root.set_margin_bottom(8);
        let window = gtk::Window::builder()
            .default_width(360)
            .default_height(180)
            .child(&content)
            .build();
        window.present();
        let loop_ = glib::MainLoop::new(None, false);
        let finished = loop_.clone();
        glib::timeout_add_local_once(Duration::from_millis(200), move || finished.quit());
        loop_.run();
        for slots in text.slots.borrow().values() {
            for slot in slots {
                if let Some(picture) = slot.picture.upgrade() {
                    let position = text.label.layout().index_to_pos(slot.byte_start);
                    eprintln!(
                        "inline position index={} layout=({}, {}) offsets={:?} bounds={:?} margins=({}, {})",
                        slot.byte_start,
                        position.x() / gtk::pango::SCALE,
                        position.y() / gtk::pango::SCALE,
                        text.label.layout_offsets(),
                        text.label.compute_bounds(&text.root),
                        picture.margin_start(),
                        picture.margin_top(),
                    );
                }
            }
        }
        assert!(short.root.height() < 50, "short inline text split into rows");
        assert!(text.root.height() < 100, "inline paragraph grew separate rows");
        let snapshot = gtk::Snapshot::new();
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        paintable.snapshot(&snapshot, 360., 180.);
        if let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) {
            renderer
                .render_texture(
                    &node,
                    Some(&gtk::graphene::Rect::new(0., 0., 360., 180.)),
                )
                .save_to_png(".state/inline-flow-after.png")
                .unwrap();
        }
        window.close();
    }
}

mod inline_overlay_imp {
    use super::*;

    pub struct Child {
        pub widget: gtk::Widget,
        pub x: std::cell::Cell<i32>,
        pub y: std::cell::Cell<i32>,
    }

    #[derive(Default)]
    pub struct InlineOverlay {
        pub children: RefCell<Vec<Child>>,
        pub reposition: RefCell<Option<Box<dyn Fn()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InlineOverlay {
        const NAME: &'static str = "MonogramInlineEmojiOverlay";
        type Type = super::InlineOverlay;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for InlineOverlay {
        fn dispose(&self) {
            for child in self.children.borrow_mut().drain(..) {
                child.widget.unparent();
            }
        }
    }
    impl WidgetImpl for InlineOverlay {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.children
                .borrow()
                .first()
                .map(|child| child.widget.measure(orientation, for_size))
                .unwrap_or((0, 0, -1, -1))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let children = self.children.borrow();
            let Some(primary) = children.first() else {
                return;
            };
            primary.widget.allocate(width, height, baseline, None);
            if let Some(reposition) = self.reposition.borrow().as_ref() {
                reposition();
            }
            for child in children.iter().skip(1) {
                let (_, natural_width, _, _) = child
                    .widget
                    .measure(gtk::Orientation::Horizontal, -1);
                let (_, natural_height, _, _) =
                    child.widget.measure(gtk::Orientation::Vertical, natural_width);
                let transform = gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(
                    child.x.get() as f32,
                    child.y.get() as f32,
                ));
                child
                    .widget
                    .allocate(natural_width, natural_height, -1, Some(transform));
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            for child in self.children.borrow().iter() {
                self.obj().snapshot_child(&child.widget, snapshot);
            }
        }
    }
}

glib::wrapper! {
    pub struct InlineOverlay(ObjectSubclass<inline_overlay_imp::InlineOverlay>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl InlineOverlay {
    fn new() -> Self {
        glib::Object::new()
    }

    fn set_child(&self, child: &impl IsA<gtk::Widget>) {
        let child = child.clone().upcast::<gtk::Widget>();
        child.set_parent(self);
        self.imp().children.borrow_mut().insert(
            0,
            inline_overlay_imp::Child {
                widget: child,
                x: std::cell::Cell::new(0),
                y: std::cell::Cell::new(0),
            },
        );
    }

    fn add_overlay(&self, child: &impl IsA<gtk::Widget>) {
        let child = child.clone().upcast::<gtk::Widget>();
        child.set_parent(self);
        self.imp()
            .children
            .borrow_mut()
            .push(inline_overlay_imp::Child {
                widget: child,
                x: std::cell::Cell::new(0),
                y: std::cell::Cell::new(0),
            });
    }

    fn set_overlay_position(&self, widget: &impl IsA<gtk::Widget>, x: i32, y: i32) {
        let widget = widget.upcast_ref::<gtk::Widget>();
        if let Some(child) = self
            .imp()
            .children
            .borrow()
            .iter()
            .find(|child| child.widget == *widget)
        {
            child.x.set(x);
            child.y.set(y);
        }
    }
}

/// A retained inline renderer. Cached assets can arrive independently without
/// rebuilding the message list; only this compact run is refreshed.
pub(crate) struct InlineMessageText {
    pub(crate) root: InlineOverlay,
    label: gtk::Label,
    body: String,
    emoji: InlineEmoji,
    textures: RefCell<HashMap<String, gdk::Texture>>,
    slots: RefCell<HashMap<String, Vec<InlineSlot>>>,
}

struct InlineSlot {
    picture: glib::WeakRef<gtk::Picture>,
    byte_start: i32,
}

impl InlineMessageText {
    pub(crate) fn new(
        body: &str,
        emoji: &InlineEmoji,
        status: Option<&gtk::Box>,
    ) -> Rc<Self> {
        let label = crate::widgets::wrap(body, "message-text");
        label.set_selectable(true);
        label.update_property(&[gtk::accessible::Property::Label(body)]);
        let root = InlineOverlay::new();
        if let Some(status) = status {
            root.set_child(&crate::widgets::MessageText::new(&label, status));
        } else {
            root.set_child(&label);
        }
        root.add_css_class("inline-emoji-text");
        let this = Rc::new(Self {
            root,
            label,
            body: body.to_owned(),
            emoji: emoji.clone(),
            textures: RefCell::new(HashMap::new()),
            slots: RefCell::new(HashMap::new()),
        });
        this.refresh();
        let weak = Rc::downgrade(&this);
        this.root.connect_map(move |_| {
            if let Some(this) = weak.upgrade() {
                this.reposition();
            }
        });
        let weak = Rc::downgrade(&this);
        this.root.imp().reposition.replace(Some(Box::new(move || {
            if let Some(this) = weak.upgrade() {
                this.reposition();
            }
        })));
        this
    }

    pub(crate) fn set_texture(&self, cid: &str, texture: gdk::Texture) {
        self.textures
            .borrow_mut()
            .insert(cid.to_owned(), texture.clone());
        self.refresh_attributes();
        if let Some(slots) = self.slots.borrow().get(cid) {
            for slot in slots {
                if let Some(picture) = slot.picture.upgrade() {
                    picture.set_paintable(Some(&texture));
                    picture.set_visible(true);
                }
            }
        }
        self.reposition();
    }

    fn refresh(&self) {
        self.slots.borrow_mut().clear();
        let byte_offsets = self
            .body
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(self.body.len()))
            .collect::<Vec<_>>();
        for range in &self.emoji.ranges {
            let start = *byte_offsets.get(range.start).unwrap_or(&self.body.len());
            let end = *byte_offsets.get(range.end).unwrap_or(&self.body.len());
            let fallback = &self.body[start..end];
            let texture = self.textures.borrow().get(&range.asset.cid).cloned();
            let picture = gtk::Picture::new();
            picture.set_size_request(20, 20);
            picture.set_halign(gtk::Align::Start);
            picture.set_valign(gtk::Align::Start);
            picture.set_can_shrink(true);
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_tooltip_text(Some(&fallback));
            picture.update_property(&[gtk::accessible::Property::Label(&fallback)]);
            picture.add_css_class("inline-custom-emoji");
            picture.set_paintable(texture.as_ref());
            picture.set_visible(texture.is_some());
            self.root.add_overlay(&picture);
            self.slots
                .borrow_mut()
                .entry(range.asset.cid.clone())
                .or_default()
                .push(InlineSlot {
                    picture: picture.downgrade(),
                    byte_start: start as i32,
                });
        }
        self.refresh_attributes();
        self.reposition();
    }

    fn refresh_attributes(&self) {
        let attributes = self.label.attributes().unwrap_or_default();
        for range in &self.emoji.ranges {
            if !self.textures.borrow().contains_key(&range.asset.cid) {
                continue;
            }
            let start = self
                .body
                .char_indices()
                .nth(range.start)
                .map(|(index, _)| index)
                .unwrap_or(self.body.len());
            let end = self
                .body
                .char_indices()
                .nth(range.end)
                .map(|(index, _)| index)
                .unwrap_or(self.body.len());
            let ink = gtk::pango::Rectangle::new(0, 0, 0, 0);
            let logical = gtk::pango::Rectangle::new(
                0,
                -16 * gtk::pango::SCALE,
                20 * gtk::pango::SCALE,
                20 * gtk::pango::SCALE,
            );
            let mut shape = gtk::pango::AttrShape::new(&ink, &logical);
            shape.set_start_index(start as u32);
            shape.set_end_index(end as u32);
            attributes.insert(shape);
        }
        self.label.set_attributes(Some(&attributes));
    }

    fn reposition(&self) {
        let layout = self.label.layout();
        let (layout_x, layout_y) = self.label.layout_offsets();
        let Some(label_bounds) = self.label.compute_bounds(&self.root) else {
            return;
        };
        for slots in self.slots.borrow().values() {
            for slot in slots {
                let Some(picture) = slot.picture.upgrade() else {
                    continue;
                };
                let position = layout.index_to_pos(slot.byte_start);
                let x = label_bounds.x() as i32
                    + layout_x
                    + position.x() / gtk::pango::SCALE;
                let y = label_bounds.y() as i32
                    + layout_y
                    + position.y() / gtk::pango::SCALE
                    + (position.height() / gtk::pango::SCALE - 20).max(0) / 2;
                let (x, y) = (x.max(0), y.max(0));
                self.root.set_overlay_position(&picture, x, y);
            }
        }
    }
}

fn inline_lottie_visible(renderer: &InlineMessageText) -> bool {
    if !renderer.root.is_mapped() {
        return false;
    }
    let Some(scroll) = renderer
        .root
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_then(|widget| widget.downcast::<gtk::ScrolledWindow>().ok())
    else {
        return true;
    };
    renderer.root.compute_bounds(&scroll).is_some_and(|bounds| {
        bounds.y() + bounds.height() >= 0. && bounds.y() <= scroll.height() as f32
    })
}

fn wait_inline_lottie(
    renderer: std::rc::Weak<InlineMessageText>,
    cid: String,
    source: Arc<crate::downloads::VerifiedFile>,
) {
    glib::timeout_add_local_once(Duration::from_millis(100), move || {
        let Some(current) = renderer.upgrade() else {
            return;
        };
        if inline_lottie_visible(&current) {
            start_inline_lottie(renderer, cid, source);
        } else {
            wait_inline_lottie(renderer, cid, source);
        }
    });
}

fn schedule_inline_lottie(
    renderer: std::rc::Weak<InlineMessageText>,
    cid: String,
    source: Arc<crate::downloads::VerifiedFile>,
    decoder: crate::lottie::Decoder,
    delay: Duration,
) {
    glib::timeout_add_local_once(delay.max(Duration::from_millis(16)), move || {
        let Some(current) = renderer.upgrade() else {
            return;
        };
        if !inline_lottie_visible(&current) {
            drop(decoder);
            wait_inline_lottie(renderer, cid, source);
            return;
        }
        glib::spawn_future_local(async move {
            let permit = crate::media_library_ui::lottie_permit().await;
            let result = gio::spawn_blocking(move || {
                let mut decoder = decoder;
                let frame = decoder.frame();
                (decoder, frame)
            })
            .await;
            drop(permit);
            let Ok((decoder, Ok(frame))) = result else {
                return;
            };
            let Some(current) = renderer.upgrade() else {
                return;
            };
            current.set_texture(&cid, frame.texture);
            schedule_inline_lottie(renderer, cid, source, decoder, frame.delay);
        });
    });
}

fn start_inline_lottie(
    renderer: std::rc::Weak<InlineMessageText>,
    cid: String,
    source: Arc<crate::downloads::VerifiedFile>,
) {
    glib::spawn_future_local(async move {
        let cancel = gio::Cancellable::new();
        let permit = crate::media_library_ui::lottie_permit().await;
        let source_frames = source.clone();
        let result = gio::spawn_blocking(move || {
            let mut decoder = crate::lottie::Decoder::open(source.reopen()?, 48, &cancel)?;
            let frame = decoder.frame()?;
            Ok::<_, &'static str>((decoder, frame))
        })
        .await;
        drop(permit);
        let (Some(current), Ok(Ok((decoder, frame)))) = (renderer.upgrade(), result) else {
            return;
        };
        current.set_texture(&cid, frame.texture);
        schedule_inline_lottie(renderer, cid, source_frames, decoder, frame.delay);
    });
}

fn play_inline_lottie(
    renderer: std::rc::Weak<InlineMessageText>,
    cid: String,
    verified: crate::downloads::VerifiedFile,
) {
    start_inline_lottie(renderer, cid, Arc::new(verified));
}

impl Ui {
    pub(crate) fn composer_inline_emoji(&self) -> Option<InlineEmoji> {
        self.composer_inline.value()
    }

    pub(crate) fn clear_composer_inline_emoji(&self) {
        self.composer_inline.clear();
    }

    pub(crate) fn insert_custom_emoji(&self, asset: InlineAsset, fallback: &str) -> bool {
        let inserted = self.composer_inline.insert(asset, fallback);
        if inserted {
            self.composer.grab_focus();
        }
        inserted
    }

    pub(crate) fn inline_message_text(
        self: &Rc<Self>,
        body: &str,
        emoji: &InlineEmoji,
        status: Option<&gtk::Box>,
    ) -> gtk::Widget {
        let renderer = InlineMessageText::new(body, emoji, status);
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let mut seen = std::collections::HashSet::new();
        for asset in emoji
            .ranges
            .iter()
            .map(|range| range.asset.clone())
            .filter(|asset| seen.insert(asset.cid.clone()))
        {
            let weak_ui = Rc::downgrade(self);
            let weak_renderer = Rc::downgrade(&renderer);
            let cid = asset.cid.clone();
            let owner = owner.clone();
            glib::spawn_future_local(async move {
                let Ok(library) = Library::open(&owner).await else {
                    return;
                };
                let Ok((file, verified)) = library.verified_inline(asset, Job::new()).await else {
                    return;
                };
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                if ui.generation.get() != generation || ui.state.borrow().account != owner {
                    return;
                }
                drop(ui);
                if file.mime == "application/x-tgsticker" {
                    play_inline_lottie(weak_renderer, cid, verified);
                } else if file.mime.starts_with("image/") {
                    let cancel = gio::Cancellable::new();
                    let decoded = gio::spawn_blocking(move || {
                        let mut decoder = crate::image_loader::Decoder::open(verified, &cancel)?;
                        decoder.frame(48).map(|frame| frame.texture)
                    })
                    .await;
                    let Ok(Ok(texture)) = decoded else {
                        return;
                    };
                    let (Some(ui), Some(renderer)) = (weak_ui.upgrade(), weak_renderer.upgrade())
                    else {
                        return;
                    };
                    if ui.generation.get() == generation && ui.state.borrow().account == owner {
                        renderer.set_texture(&cid, texture);
                    }
                }
            });
        }
        renderer.root.clone().upcast()
    }
}
