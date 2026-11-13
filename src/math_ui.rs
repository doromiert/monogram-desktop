//! The same JLaTeXMath typesetting family as mobile, isolated from the UI thread.
use crate::{
    inline_emoji_ui::InlineOverlay,
    markdown::{Formula, RichText},
};
use gtk::{gdk, gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    io::{Read, Write},
    process::{Command, Stdio},
    rc::Rc,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
#[derive(Clone)]
struct Image {
    texture: gdk::Texture,
    depth: i32,
}
thread_local! {static CACHE:RefCell<VecDeque<(String,Image)>>=RefCell::new(VecDeque::new());}
fn cache_get(key: &str) -> Option<Image> {
    CACHE.with(|cache| {
        cache
            .borrow()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    })
}
fn cache_put(key: String, image: Image) {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.push_back((key, image));
        while cache.len() > 128
            || cache
                .iter()
                .map(|(_, i)| i.texture.width() as usize * i.texture.height() as usize * 4)
                .sum::<usize>()
                > 32 * 1024 * 1024
        {
            cache.pop_front();
        }
    });
}
static WORKERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
fn render(source: &str, display: bool, color: u32) -> Result<(Vec<u8>, i32), String> {
    if source.is_empty() || source.encode_utf16().count() > 8192 || source.len() > 32768 {
        return Err("Formula is too large".into());
    }
    let helper = std::env::var("MONOGRAM_MATH_HELPER")
        .unwrap_or_else(|_| env!("MONOGRAM_BUILT_MATH_HELPER").into());
    let bwrap =
        std::env::var("MONOGRAM_BWRAP").unwrap_or_else(|_| env!("MONOGRAM_BUILT_BWRAP").into());
    if !std::path::Path::new(&helper).is_absolute() || !std::path::Path::new(&helper).is_file() {
        return Err("Math renderer is unavailable".into());
    }
    let mut child = Command::new(bwrap)
        .env_clear()
        .args([
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--clearenv",
            "--ro-bind",
            "/nix/store",
            "/nix/store",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--dir",
            "/tmp/empty",
            "--chdir",
            "/tmp/empty",
            "--setenv",
            "HOME",
            "/nonexistent",
            "--ro-bind",
        ])
        .arg(&helper)
        .arg("/renderer")
        .arg("/renderer")
        .arg(if display { "display" } else { "inline" })
        .arg(format!("{color:08x}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Math renderer could not start")?;
    let Some(mut writer) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Math input unavailable".into());
    };
    if writer.write_all(source.as_bytes()).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Math input failed".into());
    }
    drop(writer);
    let reader = child.stdout.take().unwrap();
    let output = std::thread::spawn(move || {
        let mut bytes = vec![];
        reader
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = output
        .join()
        .map_err(|_| "Math output failed")?
        .map_err(|_| "Math output failed")?;
    if status.is_none_or(|s| !s.success()) || bytes.len() > 8 * 1024 * 1024 {
        return Err("Formula could not be rendered".into());
    }
    let header = bytes
        .iter()
        .position(|b| *b == b'\n')
        .filter(|i| *i <= 10)
        .ok_or("Math output invalid")?;
    let depth = std::str::from_utf8(&bytes[..header])
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|n| *n >= 0 && *n <= 2048)
        .ok_or("Math depth invalid")?;
    if !bytes[header + 1..].starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("Math image invalid".into());
    }
    Ok((bytes[header + 1..].to_vec(), depth))
}
struct Slot {
    stack: glib::WeakRef<gtk::Stack>,
    picture: glib::WeakRef<gtk::Picture>,
    formula: Formula,
    byte: i32,
    width: Cell<i32>,
    height: Cell<i32>,
    depth: Cell<i32>,
}
struct InlineMath {
    root: glib::WeakRef<InlineOverlay>,
    label: glib::WeakRef<gtk::Label>,
    slots: Vec<Slot>,
    spoilers: Option<Rc<crate::formatting_ui::FormattedLabel>>,
}
impl InlineMath {
    fn shape(&self) {
        let Some(label) = self.label.upgrade() else {
            return;
        };
        let attrs = label
            .attributes()
            .and_then(|a| a.copy())
            .unwrap_or_default();
        for slot in &self.slots {
            let logical = gtk::pango::Rectangle::new(
                0,
                -(slot.height.get() - slot.depth.get()) * gtk::pango::SCALE,
                slot.width.get() * gtk::pango::SCALE,
                slot.height.get() * gtk::pango::SCALE,
            );
            let mut shape =
                gtk::pango::AttrShape::new(&gtk::pango::Rectangle::new(0, 0, 0, 0), &logical);
            shape.set_start_index(slot.byte as u32);
            shape.set_end_index(slot.byte as u32 + 3);
            attrs.change(shape);
        }
        label.set_attributes(Some(&attrs));
    }
    fn reposition(&self) {
        let (Some(root), Some(label)) = (self.root.upgrade(), self.label.upgrade()) else {
            return;
        };
        let Some(bounds) = label.compute_bounds(&root) else {
            return;
        };
        let layout = label.layout();
        let (lx, ly) = label.layout_offsets();
        for slot in &self.slots {
            let Some(stack) = slot.stack.upgrade() else {
                continue;
            };
            let hidden = self
                .spoilers
                .as_ref()
                .is_some_and(|s| s.hidden_at(slot.formula.start));
            stack.set_visible(!hidden);
            stack.update_state(&[gtk::accessible::State::Hidden(hidden)]);
            let pos = layout.index_to_pos(slot.byte);
            let mut iter = layout.iter();
            let mut baseline = iter.baseline();
            loop {
                if let Some(line) = iter.line_readonly() {
                    if slot.byte >= line.start_index()
                        && slot.byte <= line.start_index() + line.length()
                    {
                        baseline = iter.baseline();
                        break;
                    }
                }
                if !iter.next_line() {
                    break;
                }
            }
            root.set_overlay_position(
                &stack,
                bounds.x() as i32 + lx + pos.x() / gtk::pango::SCALE,
                bounds.y() as i32 + ly + baseline / gtk::pango::SCALE
                    - (slot.height.get() - slot.depth.get()),
            );
        }
    }
    fn load(self: &Rc<Self>, index: usize) {
        let state = self.clone();
        glib::spawn_future_local(async move {
            let permit = WORKERS
                .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
                .clone()
                .acquire_owned()
                .await;
            let Ok(_permit) = permit else {
                return;
            };
            let Some(label) = state.label.upgrade() else {
                return;
            };
            let color = label.color();
            let color = ((color.alpha() * 255.).round() as u32) << 24
                | ((color.red() * 255.).round() as u32) << 16
                | ((color.green() * 255.).round() as u32) << 8
                | (color.blue() * 255.).round() as u32;
            let slot = &state.slots[index];
            let source = slot.formula.source.clone();
            let display = slot.formula.display;
            let key = format!("{color}:{display}:{source}");
            let image = if let Some(image) = cache_get(&key) {
                Ok(image)
            } else {
                let raw = gio::spawn_blocking(move || render(&source, display, color)).await;
                match raw {
                    Ok(Ok((bytes, depth))) => {
                        gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes))
                            .map(|texture| Image { texture, depth })
                            .map_err(|_| "Math image invalid".to_string())
                    }
                    _ => Err("Formula could not be rendered".into()),
                }
            };
            let slot = &state.slots[index];
            let Some(stack) = slot.stack.upgrade() else {
                return;
            };
            if let Ok(image) = image {
                let width = (image.texture.width() + 1) / 2;
                let height = (image.texture.height() + 1) / 2;
                slot.width.set(width);
                slot.height.set(height);
                slot.depth.set((image.depth + 1) / 2);
                if let Some(picture) = slot.picture.upgrade() {
                    picture.set_paintable(Some(&image.texture));
                    picture.set_size_request(width, height);
                }
                stack.set_visible_child_name("image");
                cache_put(key, image);
            } else {
                let fallback = crate::widgets::wrap(&slot.formula.source, "monospace");
                fallback.set_max_width_chars(40);
                stack.add_named(&fallback, Some("source"));
                stack.set_visible_child_name("source");
                let width = slot
                    .formula
                    .source
                    .chars()
                    .count()
                    .saturating_mul(8)
                    .clamp(24, 320) as i32;
                slot.width.set(width);
                slot.height.set(24);
                slot.depth.set(4);
            }
            stack.set_size_request(slot.width.get(), slot.height.get());
            if let Some(root) = state.root.upgrade() {
                root.set_overlay_size(&stack, slot.width.get(), slot.height.get());
            }
            state.shape();
            state.reposition();
        });
    }
}
pub fn inline_math(
    label: &gtk::Label,
    rich: &RichText,
    spoilers: Option<Rc<crate::formatting_ui::FormattedLabel>>,
) -> gtk::Widget {
    let root = InlineOverlay::new();
    root.set_child(label);
    let mut slots = vec![];
    for formula in &rich.math {
        let stack = gtk::Stack::new();
        stack.set_size_request(24, 24);
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        spinner.set_tooltip_text(Some("Rendering formula"));
        stack.add_named(&spinner, Some("loading"));
        let picture = gtk::Picture::new();
        picture.set_can_shrink(true);
        picture.set_tooltip_text(Some(&formula.source));
        picture.update_property(&[gtk::accessible::Property::Label(&formula.source)]);
        stack.add_named(&picture, Some("image"));
        stack.set_visible_child_name("loading");
        root.add_overlay(&stack);
        slots.push(Slot {
            stack: stack.downgrade(),
            picture: picture.downgrade(),
            formula: formula.clone(),
            byte: rich
                .text
                .char_indices()
                .nth(formula.start)
                .map_or(0, |(i, _)| i as i32),
            width: Cell::new(24),
            height: Cell::new(24),
            depth: Cell::new(4),
        });
    }
    let state = Rc::new(InlineMath {
        root: root.downgrade(),
        label: label.downgrade(),
        slots,
        spoilers,
    });
    state.shape();
    let positioned = state.clone();
    root.on_reposition(move || positioned.reposition());
    let positioned = state.clone();
    label.connect_notify_local(Some("label"), move |_, _| positioned.reposition());
    for i in 0..state.slots.len() {
        state.load(i);
    }
    root.upcast()
}
pub fn display_math(source: &str) -> gtk::Widget {
    let rich = RichText {
        text: "\u{fffc}".into(),
        math: vec![Formula {
            start: 0,
            end: 1,
            source: source.into(),
            display: true,
        }],
        ..Default::default()
    };
    let label = crate::widgets::wrap(&rich.text, "message-text");
    let state = crate::formatting_ui::FormattedLabel::new(&label, rich.clone());
    let widget = inline_math(&label, &rich, Some(state));
    widget.add_css_class("article-math");
    widget
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires packaged JLaTeXMath helper and bubblewrap"]
    fn typeset_fraction_and_reject_resource_import() {
        let (bytes, depth) = render("\\frac{1}{2}+\\sqrt{x^2}", true, 0xffffffff).unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
        assert!(depth > 0);
        assert!(render("\\includegraphics{/etc/passwd}", true, 0xffffffff).is_err());
    }
}
