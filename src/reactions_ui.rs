// SPDX-License-Identifier: GPL-3.0-or-later
//! Reaction row transitions adapt Monogram Mobile's ReactionsLayoutInBubble
//! (0.5↔1 scale with matching opacity) and ChatListItemAnimator (250 ms with its
//! DEFAULT_INTERPOLATOR cubic Bézier curve).
//! The mobile document-driven RLottie burst is intentionally not synthesized here.
use crate::{model::Message, ui::Ui, widgets::MessageText};
use adw::prelude::*;
use gtk::{glib, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

mod shell_imp {
    use super::*;

    pub struct ReactionShell {
        pub child: RefCell<Option<gtk::Widget>>,
        pub scale: Cell<f64>,
        pub opacity: Cell<f64>,
    }

    impl Default for ReactionShell {
        fn default() -> Self {
            Self {
                child: RefCell::new(None),
                scale: Cell::new(1.),
                opacity: Cell::new(1.),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ReactionShell {
        const NAME: &'static str = "MonogramReactionShell";
        type Type = super::ReactionShell;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ReactionShell {
        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for ReactionShell {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.child
                .borrow()
                .as_ref()
                .map_or((0, 0, -1, -1), |child| child.measure(orientation, for_size))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.child.borrow().as_ref() {
                child.allocate(width, height, baseline, None);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(child) = self.child.borrow().as_ref().cloned() else {
                return;
            };
            let scale = self.scale.get() as f32;
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(
                self.obj().width() as f32 / 2.,
                self.obj().height() as f32 / 2.,
            ));
            snapshot.scale(scale, scale);
            snapshot.translate(&gtk::graphene::Point::new(
                self.obj().width() as f32 / -2.,
                self.obj().height() as f32 / -2.,
            ));
            snapshot.push_opacity(self.opacity.get());
            self.obj().snapshot_child(&child, snapshot);
            snapshot.pop();
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct ReactionShell(ObjectSubclass<shell_imp::ReactionShell>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ReactionShell {
    fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let shell: Self = glib::Object::new();
        let child = child.clone().upcast::<gtk::Widget>();
        child.set_parent(&shell);
        shell.imp().child.replace(Some(child));
        shell
    }

    fn set_frame(&self, scale: f64, opacity: f64) {
        self.imp().scale.set(scale);
        self.imp().opacity.set(opacity);
        self.queue_draw();
    }
}

struct ReactionItem {
    emoji: String,
    shell: ReactionShell,
    button: gtk::ToggleButton,
    count: gtk::Label,
    shown: Cell<bool>,
    generation: Cell<u64>,
    animation: RefCell<Option<adw::TimedAnimation>>,
}

// Articles and media put status outside MessageText; retain its exact original home.
struct StatusHome {
    status:gtk::Box,slot:gtk::Box,parent:glib::WeakRef<gtk::Widget>,
    previous:glib::WeakRef<gtk::Widget>,sticker:bool,moved:Cell<bool>,
}
impl StatusHome {
    fn new(status:&gtk::Box,row:&gtk::Box)->Self {
        let slot=gtk::Box::new(gtk::Orientation::Horizontal,0);
        slot.add_css_class("reaction-status");slot.set_hexpand(true);slot.set_halign(gtk::Align::End);slot.set_valign(gtk::Align::End);slot.set_margin_start(8);slot.set_visible(false);
        let parent=status.parent().map(|p|p.downgrade()).unwrap_or_default();
        let previous=status.prev_sibling().map(|p|p.downgrade()).unwrap_or_default();
        row.append(&slot);
        Self{status:status.clone(),slot,parent,previous,sticker:status.has_css_class("sticker-meta"),moved:Cell::new(false)}
    }
    fn sync(&self,shown:bool){
        if self.moved.get()==shown{return;}
        if shown {
            match self.status.parent(){
                Some(parent)if parent.is::<gtk::Overlay>()=>parent.downcast::<gtk::Overlay>().unwrap().remove_overlay(&self.status),
                Some(parent)if parent.is::<gtk::Box>()=>parent.downcast::<gtk::Box>().unwrap().remove(&self.status),
                None=>{},_=>return,
            }
            self.status.remove_css_class("sticker-meta");self.slot.append(&self.status);self.slot.set_visible(true);
        }else{
            self.slot.remove(&self.status);self.slot.set_visible(false);
            if self.sticker{self.status.add_css_class("sticker-meta");}
            if let Some(parent)=self.parent.upgrade(){
                if let Some(overlay)=parent.downcast_ref::<gtk::Overlay>(){overlay.add_overlay(&self.status);}
                else if let Some(container)=parent.downcast_ref::<gtk::Box>(){let previous=self.previous.upgrade().filter(|p|p.parent().as_ref()==Some(&parent));container.insert_child_after(&self.status,previous.as_ref());}
            }
        }
        self.moved.set(shown);
    }
}

struct ReactionsInner {
    ui: Weak<Ui>,
    row: gtk::Box,
    bubble: gtk::Box,
    items: RefCell<Vec<Rc<ReactionItem>>>,
    updating: Rc<Cell<bool>>,
    next_generation: Cell<u64>,
    status_home:Option<StatusHome>,
}

pub(crate) struct MessageReactions(Rc<ReactionsInner>);

impl MessageReactions {
    pub(crate) fn new(ui: &Rc<Ui>, message: &Message, bubble: &gtk::Box, media: bool, status:&gtk::Box) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.add_css_class("message-reactions");
        row.set_margin_top(if media && !message.text.is_empty() {
            3
        } else {
            8
        });
        if media {
            let horizontal = if message.text.is_empty() { 5 } else { 11 };
            row.set_margin_start(horizontal);
            row.set_margin_end(horizontal);
            row.set_margin_bottom(5);
        }
        let status_home=(!MessageText::attach_reaction_row(bubble,&row)).then(||StatusHome::new(status,&row));
        bubble.append(&row);
        let reactions = Self(Rc::new(ReactionsInner {
            ui: Rc::downgrade(ui),
            row,
            bubble: bubble.clone(),
            items: RefCell::new(Vec::new()),
            updating: Rc::new(Cell::new(false)),
            next_generation: Cell::new(0),
            status_home,
        }));
        reactions.sync(ui, message, false);
        reactions
    }

    pub(crate) fn update(&self, ui: &Rc<Ui>, message: &Message) {
        self.sync(ui, message, true);
    }

    fn sync(&self, ui: &Rc<Ui>, message: &Message, animate: bool) {
        let wanted = message
            .reactions
            .iter()
            .filter_map(|reaction| {
                let (emoji, count) = reaction.rsplit_once(' ').unwrap_or((reaction, ""));
                (!message
                    .poll
                    .as_ref()
                    .is_some_and(|poll| poll.is_option(emoji)))
                .then(|| (emoji.to_owned(), count.to_owned(), reaction.clone()))
            })
            .collect::<Vec<_>>();
        let mut previous: Option<gtk::Widget> = None;
        for (emoji, count, accessible) in &wanted {
            let existing = self
                .0
                .items
                .borrow()
                .iter()
                .find(|item| item.emoji == *emoji)
                .cloned();
            let item = existing.unwrap_or_else(|| self.make_item(ui, message, emoji));
            item.count.set_text(count);
            item.count.set_visible(!count.is_empty());
            item.button
                .update_property(&[gtk::accessible::Property::Label(accessible)]);
            let active = message.my_reactions.contains(emoji);
            if item.button.is_active() != active {
                self.0.updating.set(true);
                item.button.set_active(active);
                self.0.updating.set(false);
            }
            self.0
                .row
                .reorder_child_after(&item.shell, previous.as_ref());
            previous = Some(item.shell.clone().upcast());
            if !item.shown.replace(true) {
                self.animate(&item, true, animate);
            }
        }
        let wanted_emoji = wanted.iter().map(|(emoji, _, _)| emoji).collect::<Vec<_>>();
        for item in self.0.items.borrow().clone() {
            if item.shown.get() && !wanted_emoji.iter().any(|emoji| **emoji == item.emoji) {
                item.shown.set(false);
                self.animate(&item, false, animate);
            }
        }
        let visible = !wanted.is_empty();
        if let Some(home)=&self.0.status_home{home.sync(visible || !self.0.items.borrow().is_empty());}
        self.0
            .row
            .set_visible(visible || !self.0.items.borrow().is_empty());
        if visible {
            self.0.bubble.add_css_class("has-reactions");
            let bubble = self.0.bubble.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(bubble) = bubble.upgrade() {
                    MessageText::refresh_reaction_layout(&bubble);
                }
            });
        } else if !animate {
            self.0.bubble.remove_css_class("has-reactions");
        }
    }

    fn make_item(&self, ui: &Rc<Ui>, message: &Message, emoji: &str) -> Rc<ReactionItem> {
        let button = gtk::ToggleButton::new();
        button.add_css_class("reaction");
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        let emoji_label = gtk::Label::new(Some(emoji));
        emoji_label.add_css_class("reaction-emoji");
        let count = gtk::Label::new(None);
        count.add_css_class("reaction-count");
        chip.append(&emoji_label);
        chip.append(&count);
        button.set_child(Some(&chip));
        let updating = self.0.updating.clone();
        let weak = Rc::downgrade(ui);
        let id = message.id.clone();
        let reaction = emoji.to_owned();
        button.connect_toggled(move |_| {
            if !updating.get() {
                if let Some(ui) = weak.upgrade() {
                    ui.react(&id, &reaction);
                }
            }
        });
        let shell = ReactionShell::new(&button);
        shell.set_frame(0.5, 0.);
        let item = Rc::new(ReactionItem {
            emoji: emoji.to_owned(),
            shell,
            button,
            count,
            shown: Cell::new(false),
            generation: Cell::new(0),
            animation: RefCell::new(None),
        });
        self.0.items.borrow_mut().push(item.clone());
        self.0.row.append(&item.shell);
        item
    }

    fn animate(&self, item: &Rc<ReactionItem>, entering: bool, animated: bool) {
        if let Some(animation) = item.animation.borrow_mut().take() {
            animation.pause();
        }
        let generation = self.0.next_generation.get().wrapping_add(1);
        self.0.next_generation.set(generation);
        item.generation.set(generation);
        let enabled = animations_enabled(animated);
        if !enabled {
            item.shell.set_frame(
                if entering { 1. } else { 0.5 },
                if entering { 1. } else { 0. },
            );
            if !entering {
                self.remove_item(item, generation);
            }
            return;
        }
        let from_scale = item.shell.imp().scale.get();
        let from_opacity = item.shell.imp().opacity.get();
        let (to_scale, to_opacity) = if entering { (1., 1.) } else { (0.5, 0.) };
        if entering && from_opacity == 0. {
            item.shell.set_frame(0.5, 0.);
        }
        let animation = shell_animation(
            &item.shell,
            (from_scale, from_opacity),
            (to_scale, to_opacity),
        );
        let weak_inner = Rc::downgrade(&self.0);
        let weak_item = Rc::downgrade(item);
        animation.connect_done(move |_| {
            if let (Some(inner), Some(item)) = (weak_inner.upgrade(), weak_item.upgrade()) {
                item.animation.borrow_mut().take();
                if !entering && item.generation.get() == generation && !item.shown.get() {
                    finish_removal(&inner, &item);
                }
            }
        });
        item.animation.replace(Some(animation.clone()));
        animation.play();
    }

    fn remove_item(&self, item: &Rc<ReactionItem>, generation: u64) {
        if item.generation.get() != generation || item.shown.get() {
            return;
        }
        finish_removal(&self.0, item);
    }
}

fn animations_enabled(requested: bool) -> bool {
    requested && gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations())
}

fn shell_animation(shell: &ReactionShell, from: (f64, f64), to: (f64, f64)) -> adw::TimedAnimation {
    let weak = shell.downgrade();
    let animation = adw::TimedAnimation::new(
        shell,
        0.,
        1.,
        250,
        adw::CallbackAnimationTarget::new(move |progress| {
            if let Some(shell) = weak.upgrade() {
                let progress = mobile_ease(progress);
                shell.set_frame(
                    from.0 + (to.0 - from.0) * progress,
                    from.1 + (to.1 - from.1) * progress,
                );
            }
        }),
    );
    animation.set_easing(adw::Easing::Linear);
    animation
}

fn mobile_ease(progress: f64) -> f64 {
    const X1: f64 = 0.19919472913616398;
    const Y1: f64 = 0.010644531250000006;
    const X2: f64 = 0.27920937042459737;
    const Y2: f64 = 0.91025390625;
    let coordinate = |t: f64, a: f64, b: f64| {
        let inverse = 1. - t;
        3. * inverse * inverse * t * a + 3. * inverse * t * t * b + t * t * t
    };
    let derivative = |t: f64, a: f64, b: f64| {
        3. * (1. - t) * (1. - t) * a + 6. * (1. - t) * t * (b - a) + 3. * t * t * (1. - b)
    };
    let mut t = progress;
    for _ in 0..6 {
        let slope = derivative(t, X1, X2);
        if slope.abs() < 1e-7 {
            break;
        }
        t = (t - (coordinate(t, X1, X2) - progress) / slope).clamp(0., 1.);
    }
    coordinate(t, Y1, Y2)
}

fn finish_removal(inner: &ReactionsInner, item: &Rc<ReactionItem>) {
    let scroll = inner
        .ui
        .upgrade()
        .map(|ui| (ui.clone(), ui.capture_scroll_position()));
    if item.button.has_focus() {
        if let Some(next) = inner
            .items
            .borrow()
            .iter()
            .find(|candidate| candidate.shown.get() && !Rc::ptr_eq(candidate, item))
            .cloned()
        {
            next.button.grab_focus();
        } else if let Some(text) = focusable_message_text(inner.bubble.upcast_ref()) {
            text.grab_focus();
        } else if let Some(window) = item.button.root().and_downcast::<gtk::Window>() {
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        }
    }
    inner.row.remove(&item.shell);
    inner
        .items
        .borrow_mut()
        .retain(|candidate| !Rc::ptr_eq(candidate, item));
    if inner.items.borrow().is_empty() {
        inner.row.set_visible(false);
        inner.bubble.remove_css_class("has-reactions");
        if let Some(home)=&inner.status_home{home.sync(false);}
        MessageText::refresh_reaction_layout(&inner.bubble);
    }
    if let Some((ui, target)) = scroll {
        ui.queue_message_scroll(target);
    }
}

fn focusable_message_text(root: &gtk::Widget) -> Option<gtk::Widget> {
    if root.has_css_class("message-text") && root.is_focusable() {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Some(target) = focusable_message_text(&widget) {
            return Some(target);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn native_reaction_transition_frames_and_reduced_motion() {
        gtk::init().expect("native reaction test requires a display");
        let settings = gtk::Settings::default().expect("GTK settings");
        let previous = settings.is_gtk_enable_animations();
        settings.set_gtk_enable_animations(true);
        let button = gtk::Button::with_label("🔥 1");
        let shell = ReactionShell::new(&button);
        shell.set_frame(0.5, 0.);
        let window = gtk::Window::new();
        window.set_child(Some(&shell));
        window.present();
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        let animation = shell_animation(&shell, (0.5, 0.), (1., 1.));
        animation.play();
        let started = Instant::now();
        let mut frames = Vec::new();
        while started.elapsed() < Duration::from_millis(330) {
            glib::MainContext::default().iteration(false);
            frames.push((shell.imp().scale.get(), shell.imp().opacity.get()));
            std::thread::sleep(Duration::from_millis(8));
        }
        assert!(
            frames
                .iter()
                .filter(|(scale, opacity)| *scale > 0.5
                    && *scale < 1.
                    && *opacity > 0.
                    && *opacity < 1.)
                .count()
                >= 4,
            "reaction transition did not expose multiple intermediate frames: {frames:?}"
        );
        let final_frame = frames.last().copied().unwrap();
        assert!((final_frame.0 - 1.).abs() < 0.001 && (final_frame.1 - 1.).abs() < 0.001);
        settings.set_gtk_enable_animations(false);
        assert!(!animations_enabled(true));
        shell.set_frame(1., 1.);
        assert_eq!(
            (shell.imp().scale.get(), shell.imp().opacity.get()),
            (1., 1.)
        );
        settings.set_gtk_enable_animations(previous);
        window.close();
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use crate::widgets::{asset,bx,text,wrap};
    use std::time::Duration;
    #[test]
    #[ignore="requires private native GTK and Secret Service"]
    fn native_timestamp_uses_any_reaction_and_restores_text_media_and_article_home(){
        let app=adw::Application::builder().application_id("com.negzero.MonogramReactionStatusTest").flags(gtk::gio::ApplicationFlags::NON_UNIQUE).build();app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui=Ui::new(&app,true);adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let css=gtk::CssProvider::new();css.load_from_path(asset("style.css"));gtk::style_context_add_provider_for_display(&gtk::gdk::Display::default().unwrap(),&css,gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let root=bx(true,16);crate::widgets::margins(&root,20);let mut cases=vec![];
        for kind in 0..4 {
            let bubble=bx(true,0);bubble.add_css_class("message-bubble");if kind!=0{bubble.set_width_request(420);}bubble.set_halign(gtk::Align::Start);if kind%2==1{bubble.add_css_class("own");}
            let status=bx(false,4);status.add_css_class("message-meta");status.append(&text("20:49 ✓",""));
            if kind<2 {
                if kind==1{bubble.append(&wrap("A reply preview that makes this short message wider.","message-reply"));}
                let source=if kind==0{"A short message"}else{"Yes"};bubble.append(&MessageText::with_rich(&wrap(source,"message-text"),&status,crate::markdown::RichText::plain(source)));
            }else if kind==2 {
                bubble.add_css_class("media-bubble");let overlay=gtk::Overlay::new();let media=gtk::DrawingArea::builder().content_width(320).content_height(110).build();overlay.set_child(Some(&media));status.add_css_class("sticker-meta");status.set_halign(gtk::Align::End);status.set_valign(gtk::Align::End);overlay.add_overlay(&status);bubble.append(&overlay);
            }else{
                let article=bx(true,4);article.append(&text("An article heading","heading"));article.append(&wrap("Formatted content without a MessageText wrapper.","message-text"));article.append(&status);bubble.append(&article);
            }
            let original=status.parent().unwrap();let previous=status.prev_sibling();
            let message=Message{id:format!("status-{kind}"),text:"Message".into(),kind:"text".into(),..Default::default()};
            let reactions=MessageReactions::new(&ui,&message,&bubble,kind==2,&status);if kind==0{root.append(&crate::widgets::MessageBubbleLimit::new(&bubble,false));}else{root.append(&bubble);}cases.push((bubble,status,original,previous,reactions,message));
        }
        ui.window.set_content(Some(&root));ui.window.set_default_size(600,780);ui.window.present();
        glib::MainContext::default().block_on(async{
            glib::timeout_future(Duration::from_millis(120)).await;
            let short_body=focusable_message_text(cases[0].0.upcast_ref()).unwrap().downcast::<gtk::Label>().unwrap();
            assert_eq!(short_body.layout().line_count(),1,"short message initially wrapped");
            let initial_short_height=cases[0].0.height();
            for(_,_,_,_,reactions,message)in &mut cases{message.reactions=vec!["🔥 1".into()];reactions.update(&ui,message);}
            glib::timeout_future(Duration::from_millis(300)).await;
            for(bubble,status,_,_,reactions,_)in &cases{MessageText::refresh_reaction_layout(bubble);assert!(status.parent().is_some_and(|p|p.has_css_class("reaction-status")),"timestamp did not join reactions");assert!(status.compute_bounds(&reactions.0.row).is_some());assert!(!status.has_css_class("sticker-meta"));}
            if let Some(path)=std::env::var_os("MONOGRAM_REACTION_STATUS_CAPTURE"){let snapshot=gtk::Snapshot::new();gtk::WidgetPaintable::new(Some(&ui.window)).snapshot(&snapshot,ui.window.width() as f64,ui.window.height() as f64);ui.window.renderer().unwrap().render_texture(&snapshot.to_node().unwrap(),None).save_to_png(path).unwrap();}
            for(_,_,_,_,reactions,message)in &mut cases{message.reactions.clear();reactions.update(&ui,message);}
            glib::timeout_future(Duration::from_millis(300)).await;
            for(kind,(bubble,status,original,previous,_,_))in cases.iter().enumerate(){MessageText::refresh_reaction_layout(bubble);assert_eq!(status.parent().as_ref(),Some(original));assert_eq!(status.prev_sibling(),*previous);assert_eq!(status.has_css_class("sticker-meta"),kind==2);}
            assert_eq!(short_body.layout().line_count(),1,"short message stayed split after last reaction removal");
            assert_eq!(cases[0].0.height(),initial_short_height,"short bubble did not return to its original height");
            println!("PASS timestamp on every nonempty reaction row and original home restored for text, replies, media and articles");
        });ui.window.close();
    }
}
