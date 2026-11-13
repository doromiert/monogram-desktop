//! Native selectable formatting and explicit spoiler reveal, using mobile's metadata.
use crate::{
    formatting::{Display, Markup, Range, Style},
    markdown::{Block, Document, Link, RichText},
    model::Message,
    widgets::*,
};
use adw::prelude::*;
use gtk::{gdk, glib, prelude::*};
use std::{cell::RefCell, collections::BTreeSet, rc::Rc};

pub fn model(message: &Message) -> Markup {
    let mut model = message
        .formatting
        .clone()
        .filter(|m| m.body == message.text && m.valid())
        .unwrap_or_else(|| Markup::plain(&message.text));
    if let Some(context) = message
        .preview_spoilers
        .as_ref()
        .filter(|c| c.matches(&message.text))
    {
        model.spoiler |= context.whole;
        if model.ranges.is_empty() {
            model.ranges = context
                .ranges
                .iter()
                .map(|r| Range::new(r.start, r.end, Style::Spoiler))
                .collect();
        }
    }
    model
}
pub fn display(message: &Message) -> (Display, RichText) {
    let model = model(message);
    let display = model.display();
    let mut rich = RichText {
        text: display.body.clone(),
        ranges: display.ranges.clone(),
        ..Default::default()
    };
    if model.spoiler && !rich.text.is_empty() {
        rich.ranges
            .push(Range::new(0, rich.text.chars().count(), Style::Spoiler));
    }
    rich.links = crate::link_previews::clickable_urls(&rich.text, None)
        .into_iter()
        .map(|link| Link {
            start: rich.text[..link.range.start].chars().count(),
            end: rich.text[..link.range.end].chars().count(),
            uri: link.uri,
        })
        .collect();
    rich.links.extend(display.mentions.iter().map(|m| Link {
        start: m.start,
        end: m.end,
        uri: format!("xmpp:{}", m.address),
    }));
    (display, rich)
}
pub fn article(message: &Message) -> Option<Document> {
    let model = model(message);
    let (_, rich) = display(message);
    if rich
        .ranges
        .iter()
        .any(|r| r.styles.iter().any(|s| s.block()))
    {
        return Some(Document {
            blocks: styled_blocks(&rich, 0, rich.text.chars().count(), &[], 0),
            rich: true,
        });
    }
    if message.attachment.is_some()
        || model.spoiler
        || model.ranges.iter().any(|r| r.has(Style::Spoiler))
        || message.poll.is_some()
    {
        return None;
    }
    if !model.markdown && (!model.ranges.is_empty() || model.display().body != model.body) {
        return None;
    }
    let doc = crate::markdown::parse(&model.body)?;
    (model.markdown || doc.rich).then_some(doc)
}

pub struct FormattedLabel {
    label: glib::WeakRef<gtk::Label>,
    pub rich: RichText,
    suffix: RefCell<String>,
    revealed: RefCell<BTreeSet<usize>>,
    on_reveal: RefCell<Vec<Box<dyn Fn()>>>,
}
impl FormattedLabel {
    pub fn new(label: &gtk::Label, rich: RichText) -> Rc<Self> {
        label.add_css_class("formatted-message-text");
        label.set_selectable(true);
        let this = Rc::new(Self {
            label: label.downgrade(),
            rich,
            suffix: RefCell::new(String::new()),
            revealed: RefCell::new(BTreeSet::new()),
            on_reveal: RefCell::new(vec![]),
        });
        let state = this.clone();
        label.connect_activate_link(move |label, uri| {
            if let Some(index) = uri
                .strip_prefix("monogram-spoiler:")
                .and_then(|s| s.parse::<usize>().ok())
            {
                state.reveal(index);
                if let Some(widget) = label
                    .ancestor(MessageText::static_type())
                    .and_downcast::<MessageText>()
                {
                    widget.refresh_shape();
                }
                return glib::Propagation::Stop;
            }
            if let Some(anchor) = uri.strip_prefix('#') {
                reveal_anchor(label, anchor);
                return glib::Propagation::Stop;
            }
            launch_uri(label, uri);
            glib::Propagation::Stop
        });
        let state = this.clone();
        label.connect_copy_clipboard(move |label| {
            if let Some((a, b)) = label.selection_bounds() {
                let (a, b) = (a.min(b).max(0) as usize, a.max(b).max(0) as usize);
                label.clipboard().set_text(&state.copy_text(a, b));
                label.stop_signal_emission_by_name("copy-clipboard");
            }
        });

        this.render();
        this
    }
    pub fn on_reveal(&self, callback: impl Fn() + 'static) {
        self.on_reveal.borrow_mut().push(Box::new(callback));
    }
    pub fn reveal_all(&self) {
        let mut revealed = self.revealed.borrow_mut();
        let old = revealed.len();
        revealed.extend(
            self.rich
                .ranges
                .iter()
                .enumerate()
                .filter(|(_, r)| r.has(Style::Spoiler))
                .map(|(i, _)| i),
        );
        let changed = old != revealed.len();
        drop(revealed);
        if changed {
            self.render();
            for callback in self.on_reveal.borrow().iter() {
                callback();
            }
        }
    }
    fn reveal(&self, index: usize) {
        let Some(range) = self
            .rich
            .ranges
            .get(index)
            .filter(|r| r.has(Style::Spoiler))
        else {
            return;
        };
        let (mut start, mut end) = (range.start, range.end);
        loop {
            let previous = (start, end);
            for r in self.rich.ranges.iter().filter(|r| r.has(Style::Spoiler)) {
                if r.start < end && start < r.end {
                    start = start.min(r.start);
                    end = end.max(r.end);
                }
            }
            if previous == (start, end) {
                break;
            }
        }
        self.revealed.borrow_mut().extend(
            self.rich
                .ranges
                .iter()
                .enumerate()
                .filter(|(_, r)| r.has(Style::Spoiler) && r.start < end && start < r.end)
                .map(|(i, _)| i),
        );
        self.render();
        for callback in self.on_reveal.borrow().iter() {
            callback();
        }
    }
    pub fn with_suffix(&self, suffix: &str) {
        self.suffix.replace(suffix.into());
        self.render();
    }
    pub fn hidden_at(&self, at: usize) -> bool {
        self.rich
            .ranges
            .iter()
            .enumerate()
            .any(|(i, r)| r.start <= at && at < r.end && self.hidden(i))
    }
    fn hidden(&self, index: usize) -> bool {
        self.rich
            .ranges
            .get(index)
            .is_some_and(|r| r.has(Style::Spoiler))
            && !self.revealed.borrow().contains(&index)
    }
    pub fn copy_text(&self, start: usize, end: usize) -> String {
        let mut out = String::new();
        let mut hidden = false;
        for (at, c) in self
            .rich
            .text
            .chars()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            if self
                .rich
                .ranges
                .iter()
                .enumerate()
                .any(|(i, r)| self.hidden(i) && r.start <= at && at < r.end)
            {
                if !hidden {
                    out.push_str("[Spoiler]");
                }
                hidden = true;
            } else {
                hidden = false;
                if let Some(math) = self.rich.math.iter().find(|m| m.start == at) {
                    out.push_str(&math.source);
                } else {
                    out.push(c);
                }
            }
        }
        out
    }
    pub fn render(&self) {
        let Some(label) = self.label.upgrade() else {
            return;
        };
        let attrs = label.attributes();
        let revealed = self.revealed.borrow();
        label.set_markup(&markup(&self.rich, &revealed, &self.suffix.borrow()));
        label.set_attributes(attrs.as_ref());
        label.update_property(&[gtk::accessible::Property::Label(
            &self.copy_text(0, self.rich.text.chars().count()),
        )]);
    }
}
fn markup(rich: &RichText, revealed: &BTreeSet<usize>, suffix: &str) -> String {
    let offsets: Vec<_> = rich
        .text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(rich.text.len()))
        .collect();
    let n = offsets.len() - 1;
    let mut points = BTreeSet::from([0, n]);
    for r in &rich.ranges {
        if r.start < r.end && r.end <= n {
            points.extend([r.start, r.end]);
        }
    }
    for l in &rich.links {
        if l.start < l.end && l.end <= n {
            points.extend([l.start, l.end]);
        }
    }
    let points: Vec<_> = points.into_iter().collect();
    let mut out = String::new();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let active = rich
            .ranges
            .iter()
            .filter(|r| r.start <= a && r.end >= b)
            .flat_map(|r| r.styles.iter().copied())
            .collect::<BTreeSet<_>>();
        let hidden = rich
            .ranges
            .iter()
            .enumerate()
            .find(|(i, r)| {
                r.has(Style::Spoiler) && r.start <= a && r.end >= b && !revealed.contains(i)
            })
            .map(|(i, _)| i);
        let link = rich
            .links
            .iter()
            .find(|l| l.start <= a && l.end >= b && crate::markdown::safe_uri(&l.uri));
        if let Some(index) = hidden {
            out.push_str(&format!("<a href=\"monogram-spoiler:{index}\"><span foreground=\"#77777700\" background=\"#777777\" underline=\"none\">"));
        } else if let Some(link) = link {
            out.push_str(&format!(
                "<a href=\"{}\">",
                glib::markup_escape_text(&link.uri)
            ));
        }
        let mut closes = vec![];
        for style in active {
            let (open, close) = match style {
                Style::Strong => ("<b>", "</b>"),
                Style::Emphasis => ("<i>", "</i>"),
                Style::Deleted => ("<s>", "</s>"),
                Style::Underline => ("<u>", "</u>"),
                Style::Sub => ("<sub>", "</sub>"),
                Style::Sup => ("<sup>", "</sup>"),
                Style::Code | Style::Pre => (
                    "<span font_family=\"monospace\" background=\"#444448\">",
                    "</span>",
                ),
                Style::Quote => ("<i>", "</i>"),
                Style::Mark => ("<span background=\"#756320\">", "</span>"),
                Style::Spoiler => continue,
            };
            out.push_str(open);
            closes.push(close);
        }
        out.push_str(&glib::markup_escape_text(
            &rich.text[offsets[a]..offsets[b]],
        ));
        for close in closes.into_iter().rev() {
            out.push_str(close);
        }
        if hidden.is_some() {
            out.push_str("</span></a>");
        } else if link.is_some() {
            out.push_str("</a>");
        }
    }
    out.push_str(&glib::markup_escape_text(suffix));
    out
}
fn launch_uri(label: &gtk::Label, uri: &str) {
    if !crate::markdown::safe_uri(uri) || uri.starts_with('#') {
        return;
    }
    let parent = label.root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(uri).launch(parent.as_ref(), None::<&gtk::gio::Cancellable>, |_| {});
}
fn reveal_anchor(label: &gtk::Label, name: &str) {
    let mut root = label.parent();
    while root
        .as_ref()
        .is_some_and(|w| !w.has_css_class("formatted-article"))
    {
        root = root.and_then(|w| w.parent());
    }
    let Some(root) = root else {
        return;
    };
    let mut pending = vec![root];
    while let Some(w) = pending.pop() {
        if w.widget_name() == name {
            if let Some(expander) = w.downcast_ref::<gtk::Expander>() {
                expander.set_expanded(true);
            }
            w.grab_focus();
            return;
        }
        if let Some(list) = w.downcast_ref::<gtk::ListView>() {
            if let Some(model) = list.model() {
                for i in 0..model.n_items() {
                    if let Some(item) = model.item(i).and_downcast::<glib::BoxedAnyObject>() {
                        if matches!(&*item.borrow::<Block>(),Block::Details{title,..}if title.text.strip_prefix("Reference ").is_some_and(|id|format!("fn-{id}")==name))
                        {
                            list.scroll_to(i, gtk::ListScrollFlags::FOCUS, None);
                            return;
                        }
                    }
                }
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            pending.push(c);
        }
    }
}

pub fn article_widget(document: &Document, status: &gtk::Box) -> gtk::Widget {
    let root = bx(true, 8);
    root.add_css_class("formatted-article");
    root.set_hexpand(false);
    if document.blocks.len() > 16 {
        root.append(&virtual_blocks(&document.blocks, 520));
    } else {
        for block in &document.blocks {
            root.append(&block_widget(block));
        }
    }
    status.set_halign(gtk::Align::End);
    root.append(status);
    root.upcast()
}
fn rich_label(rich: &RichText, css: &str) -> gtk::Widget {
    let label = wrap(&rich.text, css);
    let state = FormattedLabel::new(&label, rich.clone());
    // Signal handlers retain state, with a weak label reference to avoid cycles.
    if rich.math.is_empty() {
        label.upcast()
    } else {
        crate::math_ui::inline_math(&label, rich, Some(state))
    }
}
fn block_widget(block: &Block) -> gtk::Widget {
    match block {
        Block::Paragraph(rich) => rich_label(rich, "message-text"),
        Block::Heading(level, rich) => rich_label(
            rich,
            &format!("message-text article-heading article-heading-{level}"),
        ),
        Block::Quote(blocks) => {
            let root = bx(true, 6);
            root.add_css_class("article-quote");
            for block in blocks {
                root.append(&block_widget(block));
            }
            root.upcast()
        }
        Block::Code(source, language) => {
            let root = bx(true, 4);
            root.add_css_class("article-code");
            let header = bx(false, 8);
            let caption = text(
                if language.is_empty() {
                    "Code"
                } else {
                    language
                },
                "caption dim-label",
            );
            caption.set_hexpand(true);
            header.append(&caption);
            let copy = icon_button("edit-copy-symbolic", "Copy code");
            let source = source.clone();
            let copied = source.clone();
            copy.connect_clicked(move |b| b.clipboard().set_text(&copied));
            header.append(&copy);
            root.append(&header);
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Automatic)
                .vscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .min_content_height(0)
                .build();
            let label = gtk::Label::new(Some(&source));
            label.set_xalign(0.);
            label.set_selectable(true);
            label.add_css_class("monospace");
            scroll.set_child(Some(&label));
            root.append(&scroll);
            root.upcast()
        }
        Block::List(start, items) => {
            let root = bx(true, 4);
            for (i, item) in items.iter().enumerate() {
                let row = bx(false, 8);
                row.append(&text(
                    &start
                        .as_ref()
                        .and_then(|v| v.get(i))
                        .map(|v| format!("{v}."))
                        .unwrap_or_else(|| "•".into()),
                    "article-list-marker",
                ));
                let content = bx(true, 4);
                for block in item {
                    content.append(&block_widget(block));
                }
                row.append(&content);
                root.append(&row);
            }
            root.upcast()
        }
        Block::Table(table) => {
            let rows = &table.rows;
            let grid = gtk::Grid::builder().column_homogeneous(true).build();
            grid.add_css_class("article-table");
            for (y, row) in rows.iter().enumerate() {
                for (x, cell) in row.iter().enumerate() {
                    let mut cell = cell.clone();
                    if y == 0 && !cell.text.is_empty() {
                        cell.ranges
                            .push(Range::new(0, cell.text.chars().count(), Style::Strong));
                    }
                    let widget = rich_label(&cell, "message-text article-table-cell");
                    let mut current = Some(widget.clone());
                    while let Some(w) = current {
                        if let Some(label) = w.downcast_ref::<gtk::Label>() {
                            label.set_wrap(false);
                            label.set_xalign(match table.alignment.get(x) {
                                Some(pulldown_cmark::Alignment::Center) => 0.5,
                                Some(pulldown_cmark::Alignment::Right) => 1.,
                                _ => 0.,
                            });
                            break;
                        }
                        current = w.first_child();
                    }
                    grid.attach(&widget, x as i32, y as i32, 1, 1);
                }
            }
            let scroll = horizontal_strip(&grid);
            scroll.set_min_content_height(0);
            scroll.upcast()
        }
        Block::Rule => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
        Block::Details {
            title,
            blocks,
            open,
        } => {
            let expander = gtk::Expander::new(None);
            expander.set_label_widget(Some(&rich_label(title, "heading")));
            expander.set_expanded(*open);
            if let Some(id) = title.text.strip_prefix("Reference ") {
                expander.set_widget_name(&format!("fn-{id}"));
            }
            let root = bx(true, 6);
            for block in blocks {
                root.append(&block_widget(block));
            }
            expander.set_child(Some(&root));
            expander.upcast()
        }
        Block::Math(source) => crate::math_ui::display_math(source),
    }
}

pub fn hidden_media(
    widget: &impl IsA<gtk::Widget>,
    hidden: bool,
) -> (gtk::Widget, Option<gtk::Button>) {
    let widget = widget.upcast_ref::<gtk::Widget>();
    if !hidden {
        return (widget.clone(), None);
    }
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(widget));
    let cover = gtk::Button::with_label("Reveal spoiler");
    cover.add_css_class("media-spoiler-cover");
    cover.set_halign(gtk::Align::Fill);
    cover.set_valign(gtk::Align::Fill);
    overlay.add_overlay(&cover);
    widget.set_can_target(false);
    widget.update_state(&[gtk::accessible::State::Hidden(true)]);
    let weak = widget.downgrade();
    cover.connect_clicked(move |button| {
        if let Some(widget) = weak.upgrade() {
            widget.set_can_target(true);
            widget.update_state(&[gtk::accessible::State::Hidden(false)]);
        }
        button.set_visible(false);
    });
    (overlay.upcast(), Some(cover))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spoiler_links_are_not_exposed_before_reveal() {
        let mut rich = RichText::plain("before <secret> after");
        rich.ranges.push(Range::new(7, 15, Style::Spoiler));
        rich.links.push(Link {
            start: 7,
            end: 15,
            uri: "https://example.org".into(),
        });
        let hidden = markup(&rich, &BTreeSet::new(), "");
        assert!(hidden.contains("monogram-spoiler:0"));
        assert!(!hidden.contains("https://example.org"));
        let revealed = markup(&rich, &BTreeSet::from([0]), "");
        assert!(!revealed.contains("monogram-spoiler:"));
        assert!(revealed.contains("https://example.org"));
        assert!(revealed.contains("&lt;secret&gt;"));
    }
}

const COMPOSER_STYLES: [(&str, Style, &str); 11] = [
    ("bold", Style::Strong, "Bold"),
    ("italic", Style::Emphasis, "Italic"),
    ("strike", Style::Deleted, "Strikethrough"),
    ("code", Style::Code, "Monospace"),
    ("pre", Style::Pre, "Code block"),
    ("quote", Style::Quote, "Quote"),
    ("spoiler", Style::Spoiler, "Spoiler"),
    ("underline", Style::Underline, "Underline"),
    ("mark", Style::Mark, "Highlight"),
    ("sub", Style::Sub, "Subscript"),
    ("sup", Style::Sup, "Superscript"),
];
impl crate::ui::Ui {
    pub fn install_formatting(self: &Rc<Self>) {
        install_editor(self, &self.composer, &self.composer_spoiler);
    }
    pub fn composer_formatting(&self) -> Markup {
        editor_formatting(&self.composer, &self.composer_spoiler)
    }
    pub fn restore_composer_formatting(&self, model: Option<&Markup>) {
        restore_editor_formatting(&self.composer, &self.composer_spoiler, model);
    }
}
pub fn install_editor(
    ui: &Rc<crate::ui::Ui>,
    view: &gtk::TextView,
    whole: &gtk::gio::SimpleAction,
) {
    let buffer = view.buffer();
    let group = gtk::gio::SimpleActionGroup::new();
    let menu = gtk::gio::Menu::new();
    for (name, style, title) in COMPOSER_STYLES {
        let tag = gtk::TextTag::builder()
            .name(format!("format-{name}"))
            .build();
        match style {
            Style::Strong => tag.set_weight(700),
            Style::Emphasis | Style::Quote => tag.set_style(gtk::pango::Style::Italic),
            Style::Deleted => tag.set_strikethrough(true),
            Style::Code | Style::Pre => tag.set_family(Some("monospace")),
            Style::Underline => tag.set_underline(gtk::pango::Underline::Single),
            Style::Spoiler => tag.set_background(Some("#777777")),
            Style::Mark => tag.set_background(Some("#756320")),
            Style::Sub => {
                tag.set_rise(-5000);
                tag.set_scale(0.8);
            }
            Style::Sup => {
                tag.set_rise(7000);
                tag.set_scale(0.8);
            }
        }
        buffer.tag_table().add(&tag);
        let action = gtk::gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(ui);
        let weak_view = view.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let Some(view) = weak_view.upgrade() else {
                    return;
                };
                let buffer = view.buffer();
                if let Some((a, b)) = buffer.selection_bounds() {
                    let tag = buffer
                        .tag_table()
                        .lookup(&format!("format-{name}"))
                        .unwrap();
                    if a.has_tag(&tag) {
                        buffer.remove_tag(&tag, &a, &b);
                    } else {
                        buffer.apply_tag(&tag, &a, &b);
                    }
                    ui.save_draft();
                }
            }
        });
        group.add_action(&action);
        menu.append(Some(title), Some(&format!("format.{name}")));
    }
    let all = whole.clone();
    let weak = Rc::downgrade(ui);
    let weak_view = view.downgrade();
    all.connect_activate(move |action, _| {
        let value = !action
            .state()
            .and_then(|v| v.get::<bool>())
            .unwrap_or(false);
        action.set_state(&value.to_variant());
        if let Some(ui) = weak.upgrade() {
            ui.save_draft();
        }
    });
    group.add_action(&all);
    menu.append(
        Some("Hide whole message / media as spoiler"),
        Some("format.spoiler-all"),
    );
    let clear = gtk::gio::SimpleAction::new("clear", None);
    let weak = Rc::downgrade(ui);
    let weak_view = view.downgrade();
    clear.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            let Some(view) = weak_view.upgrade() else {
                return;
            };
            let buffer = view.buffer();
            if let Some((a, b)) = buffer.selection_bounds() {
                for (name, _, _) in COMPOSER_STYLES {
                    if let Some(tag) = buffer.tag_table().lookup(&format!("format-{name}")) {
                        buffer.remove_tag(&tag, &a, &b);
                    }
                }
                ui.save_draft();
            }
        }
    });
    group.add_action(&clear);
    menu.append(Some("Clear formatting"), Some("format.clear"));
    let link = gtk::gio::SimpleAction::new("link", None);
    let weak = Rc::downgrade(ui);
    let weak_view = view.downgrade();
    link.connect_activate(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            if let Some(view) = weak_view.upgrade() {
                format_link(&ui, &view);
            }
        }
    });
    group.add_action(&link);
    menu.append(Some("Link…"), Some("format.link"));
    view.insert_action_group("format", Some(&group));
    view.set_extra_menu(Some(&menu));
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    let weak_view = view.downgrade();
    keys.connect_key_pressed(move |_, key, _, mods| {
        if !mods.contains(gdk::ModifierType::CONTROL_MASK) {
            return glib::Propagation::Proceed;
        }
        let action = match key {
            gdk::Key::b | gdk::Key::B => "bold",
            gdk::Key::i | gdk::Key::I => "italic",
            gdk::Key::u | gdk::Key::U => "underline",
            gdk::Key::k | gdk::Key::K => "link",
            _ => return glib::Propagation::Proceed,
        };
        if let Some(ui) = weak.upgrade() {
            if let Some(view) = weak_view.upgrade() {
                if view.buffer().has_selection() {
                    let _ = view.activate_action(&format!("format.{action}"), None);
                    return glib::Propagation::Stop;
                }
            }
        }
        glib::Propagation::Proceed
    });
    view.add_controller(keys);
}
pub fn editor_formatting(view: &gtk::TextView, whole: &gtk::gio::SimpleAction) -> Markup {
    let buffer = view.buffer();
    let body = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string();
    let mut spans = vec![];
    for (name, style, _) in COMPOSER_STYLES {
        let Some(tag) = buffer.tag_table().lookup(&format!("format-{name}")) else {
            continue;
        };
        let mut start = None;
        let mut iter = buffer.start_iter();
        loop {
            let at = iter.offset().max(0) as usize;
            let active = !iter.is_end() && iter.has_tag(&tag);
            if active && start.is_none() {
                start = Some(at);
            }
            if !active && let Some(begin) = start.take() {
                if begin < at {
                    spans.push(Range::new(begin, at, style));
                }
            }
            if !iter.forward_to_tag_toggle(Some(&tag)) {
                if let Some(begin) = start.take() {
                    let end = body.chars().count();
                    if begin < end {
                        spans.push(Range::new(begin, end, style));
                    }
                }
                break;
            }
        }
    }
    let mut model = Markup {
        body,
        spoiler: whole.state().and_then(|v| v.get::<bool>()).unwrap_or(false),
        ranges: crate::formatting::normalize(&spans).unwrap_or_default(),
        ..Default::default()
    };
    if !model.valid() {
        model.ranges.retain(|r| !r.styles.iter().any(|s| s.block()));
    }
    model
}
pub fn restore_editor_formatting(
    view: &gtk::TextView,
    whole: &gtk::gio::SimpleAction,
    model: Option<&Markup>,
) {
    whole.set_state(&model.is_some_and(|m| m.spoiler).to_variant());
    let Some(model) = model.filter(|m| {
        m.body
            == view
                .buffer()
                .text(&view.buffer().start_iter(), &view.buffer().end_iter(), true)
                .as_str()
            && m.valid()
    }) else {
        return;
    };
    let buffer = view.buffer();
    for range in &model.ranges {
        for (name, style, _) in COMPOSER_STYLES {
            if range.has(style)
                && let Some(tag) = buffer.tag_table().lookup(&format!("format-{name}"))
            {
                buffer.apply_tag(
                    &tag,
                    &buffer.iter_at_offset(range.start as i32),
                    &buffer.iter_at_offset(range.end as i32),
                );
            }
        }
    }
}
fn format_link(ui: &Rc<crate::ui::Ui>, view: &gtk::TextView) {
    let buffer = view.buffer();
    let Some((a, b)) = buffer.selection_bounds() else {
        return;
    };
    let start = buffer.create_mark(None, &a, true);
    let end = buffer.create_mark(None, &b, false);
    let entry = gtk::Entry::builder()
        .placeholder_text("https://example.org")
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading("Add Link")
        .extra_child(&entry)
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("add", "Add Link")]);
    dialog.set_default_response(Some("add"));
    dialog.set_close_response("cancel");
    let weak = Rc::downgrade(ui);
    dialog.connect_response(None, move |_, response| {
        if response == "add"
            && let Some(ui) = weak.upgrade()
        {
            let uri = entry.text();
            if !crate::markdown::safe_uri(&uri) {
                ui.notice("Enter a web, email, phone or XMPP link");
                return;
            }
            let a = buffer.iter_at_mark(&start);
            let b = buffer.iter_at_mark(&end);
            let selected = buffer.text(&a, &b, false).to_string();
            let replacement = format!(
                "[{}](<{}>)",
                selected.replace(']', "\\]"),
                uri.replace('<', "%3C").replace('>', "%3E")
            );
            buffer.begin_user_action();
            let mut a = a;
            let mut b = b;
            buffer.delete(&mut a, &mut b);
            buffer.insert(&mut a, &replacement);
            buffer.end_user_action();
            ui.save_draft();
        }
        buffer.delete_mark(&start);
        buffer.delete_mark(&end);
    });
    dialog.present(Some(&ui.window));
}

fn styled_blocks(
    rich: &RichText,
    start: usize,
    end: usize,
    parents: &[usize],
    depth: usize,
) -> Vec<Block> {
    if depth >= 16 {
        return vec![Block::Paragraph(rich.slice(start, end))];
    }
    let mut spans = rich
        .ranges
        .iter()
        .enumerate()
        .filter(|(i, r)| {
            r.styles.iter().any(|s| s.block())
                && r.start >= start
                && r.end <= end
                && !parents.contains(i)
        })
        .collect::<Vec<_>>();
    spans.sort_by_key(|(_, r)| (r.start, std::cmp::Reverse(r.end)));
    let mut out = vec![];
    let mut at = start;
    for (index, r) in spans {
        if r.start < at {
            continue;
        }
        if r.start > at {
            out.push(Block::Paragraph(rich.slice(at, r.start)));
        }
        let mut nested = parents.to_vec();
        nested.push(index);
        if r.has(Style::Pre)
            && !rich
                .ranges
                .iter()
                .any(|s| s.has(Style::Spoiler) && s.start < r.end && s.end > r.start)
        {
            out.push(Block::Code(
                rich.slice(r.start, r.end).text,
                r.language.clone().unwrap_or_default(),
            ));
        } else if r.has(Style::Quote) {
            out.push(Block::Quote(styled_blocks(
                rich,
                r.start,
                r.end,
                &nested,
                depth + 1,
            )));
        } else {
            out.push(Block::Paragraph(rich.slice(r.start, r.end)));
        }
        at = r.end;
    }
    if at < end {
        out.push(Block::Paragraph(rich.slice(at, end)));
    }
    out
}
fn virtual_blocks(blocks: &[Block], height: i32) -> gtk::Widget {
    let model = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
    for block in blocks {
        model.append(&glib::BoxedAnyObject::new(block.clone()));
    }
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_bind(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(value) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        item.set_child(Some(&block_widget(&value.borrow::<Block>())));
    });
    factory.connect_unbind(|_, object| {
        object
            .downcast_ref::<gtk::ListItem>()
            .unwrap()
            .set_child(gtk::Widget::NONE)
    });
    let list = gtk::ListView::new(Some(gtk::NoSelection::new(Some(model))), Some(factory));
    list.add_css_class("article-block-list");
    list.set_single_click_activate(false);
    list.set_enable_rubberband(false);
    gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(height)
        .min_content_height(120)
        .build()
        .upcast()
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn descendants(root: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
        let mut pending = vec![root.clone().upcast::<gtk::Widget>()];
        let mut result = vec![];
        while let Some(w) = pending.pop() {
            let mut c = w.first_child();
            while let Some(next) = c {
                c = next.next_sibling();
                pending.push(next);
            }
            result.push(w);
        }
        result
    }
    #[test]
    #[ignore = "requires private X11 display, xdotool, data roots and Secret Service"]
    fn mobile_formats_reveal_selection_drafts_and_native_article() {
        let app = adw::Application::builder()
            .application_id("com.negzero.MonogramFormattingTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let ui = crate::ui::Ui::new(&app, true);
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let css = gtk::CssProvider::new();
        css.load_from_path(asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let mut account = crate::model::State::live("", vec![]);
        let chat = account.chat_for("peer@fixture.invalid");
        account.active = chat.id.clone();
        ui.state.replace(account);
        assert!(
            ui.composer
                .ancestor(gtk::WindowHandle::static_type())
                .is_none(),
            "composer is inside a window-drag handle"
        );
        let frame = ui
            .composer
            .ancestor(crate::widgets::ComposerEntry::static_type())
            .unwrap();
        assert_eq!(frame.css_name(), "entry");
        let value = "😀 Bold secret link";
        ui.composer.buffer().set_text(value);
        for (name, a, b) in [("bold", 2, 6), ("spoiler", 7, 13)] {
            let buffer = ui.composer.buffer();
            buffer.select_range(&buffer.iter_at_offset(a), &buffer.iter_at_offset(b));
            ui.composer
                .activate_action(&format!("format.{name}"), None)
                .unwrap();
        }
        let authored = ui.composer_formatting();
        assert_eq!(authored.body, value);
        assert!(authored.ranges.iter().any(|r| r.has(Style::Strong)));
        assert!(authored.ranges.iter().any(|r| r.has(Style::Spoiler)));
        ui.save_draft();
        assert_eq!(
            ui.state.borrow().current().draft_formatting.as_ref(),
            Some(&authored)
        );
        ui.composer.buffer().set_text("");
        ui.composer.buffer().set_text(value);
        ui.restore_composer_formatting(Some(&authored));
        assert_eq!(ui.composer_formatting(), authored);
        ui.show_chat();
        ui.window.present();
        glib::MainContext::default().block_on(async {
            glib::timeout_future(Duration::from_millis(180)).await;
            ui.composer.grab_focus();
            let bounds = ui.composer.compute_bounds(&ui.window).unwrap();
            let point = |offset| {
                let rect = ui
                    .composer
                    .iter_location(&ui.composer.buffer().iter_at_offset(offset));
                let (x, y) = ui.composer.buffer_to_window_coords(
                    gtk::TextWindowType::Widget,
                    rect.x(),
                    rect.y(),
                );
                (
                    (bounds.x() as i32 + x + 2).to_string(),
                    (bounds.y() as i32 + y + rect.height() / 2).to_string(),
                )
            };
            let ((x1, y1), (x2, y2)) = (point(2), point(13));
            gtk::gio::spawn_blocking(move || {
                let found = std::process::Command::new("xdotool")
                    .args([
                        "search",
                        "--all",
                        "--onlyvisible",
                        "--pid",
                        &std::process::id().to_string(),
                        "--name",
                        "^Monogram$",
                    ])
                    .output()
                    .unwrap();
                let window = String::from_utf8(found.stdout)
                    .unwrap()
                    .lines()
                    .last()
                    .unwrap()
                    .to_owned();
                assert!(
                    std::process::Command::new("xdotool")
                        .args([
                            "mousemove",
                            "--sync",
                            "--window",
                            &window,
                            &x1,
                            &y1,
                            "mousedown",
                            "1",
                            "mousemove",
                            "--sync",
                            "--window",
                            &window,
                            &x2,
                            &y2,
                            "mouseup",
                            "1"
                        ])
                        .status()
                        .unwrap()
                        .success()
                );
            })
            .await
            .unwrap();
            glib::timeout_future(Duration::from_millis(80)).await;
            assert!(
                ui.composer.buffer().selection_bounds().is_some(),
                "dragging composer text failed to select text"
            );
            if let Some(path) = std::env::var_os("MONOGRAM_FORMAT_CAPTURE") {
                let snapshot = gtk::Snapshot::new();
                gtk::WidgetPaintable::new(Some(&ui.window)).snapshot(
                    &snapshot,
                    ui.window.width() as f64,
                    ui.window.height() as f64,
                );
                if let Some(node) = snapshot.to_node() {
                    ui.window
                        .renderer()
                        .unwrap()
                        .render_texture(&node, None)
                        .save_to_png(
                            std::path::Path::new(&path).with_file_name("formatting-composer.png"),
                        )
                        .unwrap();
                }
            }
        });
        let message = Message {
            text: value.into(),
            formatting: Some(authored.clone()),
            ..Default::default()
        };
        let (_, rich) = display(&message);
        let label = wrap(&rich.text, "message-text");
        let status = bx(false, 0);
        status.append(&text("12:34", "caption"));
        let message_text = MessageText::with_rich(&label, &status, rich);
        let state = message_text.formatted_state().unwrap();
        assert_eq!(state.copy_text(0, 18), "😀 Bold [Spoiler] link");
        let root = bx(true, 12);
        margins(&root, 18);
        root.append(&message_text);
        let source = "# Mobile formatting\n\n<u>Underline</u> <mark>highlight</mark> H<sub>2</sub>O x<sup>2</sup> <tg-spoiler>hidden</tg-spoiler>\n\n| Item | Count |\n| --- | --- |\n| Apples | 3 |\n\n- [x] Done\n- [ ] Next\n\n> Explicit quote\n\n```rust\nlet answer = 42;\n```\n\nInline math $\\frac{1}{2}$ and **bold**.\n\n$$\\sqrt{x^2+y^2}$$";
        let doc = crate::markdown::parse(source).unwrap();
        let article_status = bx(false, 0);
        article_status.append(&text("12:35", "caption"));
        root.append(&article_widget(&doc, &article_status));
        ui.window.set_content(Some(&root));
        ui.window.set_default_size(700, 1000);
        let start = Instant::now();
        ui.window.present();
        glib::MainContext::default().block_on(async {
            glib::timeout_future(Duration::from_millis(100)).await;assert!(start.elapsed()<Duration::from_millis(400),"native formatting blocked first feedback");
            label.select_region(0,0);
            if let Some(path)=std::env::var_os("MONOGRAM_FORMAT_CAPTURE"){let snapshot=gtk::Snapshot::new();gtk::WidgetPaintable::new(Some(&ui.window)).snapshot(&snapshot,ui.window.width() as f64,ui.window.height() as f64);if let(Some(renderer),Some(node))=(ui.window.renderer(),snapshot.to_node()){renderer.render_texture(&node,None).save_to_png(std::path::Path::new(&path).with_extension("before.png")).unwrap();}}
            println!("Root {}x{} mapped {} label {}x{} mapped {}",root.width(),root.height(),root.is_mapped(),label.width(),label.height(),label.is_mapped());
            let b=label.compute_bounds(&ui.window).unwrap();let byte=label.text().char_indices().nth(10).unwrap().0;let pos=label.layout().index_to_pos(byte as i32);let(lx,ly)=label.layout_offsets();let x=(b.x() as i32+lx+(pos.x()+pos.width()/2)/gtk::pango::SCALE).to_string();let y=(b.y() as i32+ly+(pos.y()+pos.height()/2)/gtk::pango::SCALE).to_string();
            println!("Spoiler label bounds {b:?}; click {x},{y}; layout {pos:?}");
            gtk::gio::spawn_blocking(move||{let found=std::process::Command::new("xdotool").args(["search","--all","--onlyvisible","--pid",&std::process::id().to_string(),"--name","^Monogram$"]).output().unwrap();let window=String::from_utf8(found.stdout).unwrap().lines().last().unwrap().to_owned();assert!(std::process::Command::new("xdotool").args(["mousemove","--sync","--window",&window,&x,&y,"click","1"]).status().unwrap().success());}).await.unwrap();
            glib::timeout_future(Duration::from_millis(100)).await;assert_eq!(state.copy_text(0,18),value,"native spoiler click did not reveal");label.select_region(2,6);assert_eq!(label.selection_bounds(),Some((2,6)));
            glib::timeout_future(Duration::from_millis(900)).await;
            for widget in descendants(&root){if let Some(picture)=widget.downcast_ref::<gtk::Picture>(){if picture.paintable().is_some(){let overlay=picture.ancestor(crate::inline_emoji_ui::InlineOverlay::static_type()).unwrap();let bounds=picture.compute_bounds(&overlay).unwrap();assert!(bounds.y()>=-1. && bounds.y()+bounds.height()<=overlay.height() as f32+1.,"math image escaped its text line: {bounds:?}");}}}
            let pictures=descendants(&root).into_iter().filter_map(|w|w.downcast::<gtk::Picture>().ok()).collect::<Vec<_>>();assert!(!pictures.is_empty());assert!(pictures.iter().any(|p|p.paintable().is_some()),"native math stayed at its placeholder");
            if let Some(path)=std::env::var_os("MONOGRAM_FORMAT_CAPTURE"){let snapshot=gtk::Snapshot::new();gtk::WidgetPaintable::new(Some(&ui.window)).snapshot(&snapshot,ui.window.width() as f64,ui.window.height() as f64);let image=ui.window.renderer().unwrap().render_texture(&snapshot.to_node().unwrap(),None);image.save_to_png(path).unwrap();}
            let long=crate::markdown::parse(&(0..400).map(|i|format!("## Heading {i}\n\nParagraph {i}\n\n")).collect::<String>()).unwrap();let added=Instant::now();let content=virtual_blocks(&long.blocks,320);root.append(&content);glib::timeout_future(Duration::from_millis(60)).await;assert!(added.elapsed()<Duration::from_millis(400),"long article froze native rendering");let labels=descendants(&content).iter().filter(|w|w.is::<gtk::Label>()).count();assert!(labels<300,"article blocks were not virtualized: {labels}");println!("Native spoiler click, partial selection, formatting draft, loaded math and bounded long article PASS ({} labels)",labels);
        });
        ui.window.close();
    }
}
