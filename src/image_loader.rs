//! Decode verified attachment descriptors and in-memory avatars through libglycin's Linux sandbox.
//! Construct and advance decoders on a worker; only immutable textures cross to GTK.
use gtk::{cairo, gdk, gio, glib, glib::translate::*, prelude::*};
use std::{ffi::CStr, os::fd::IntoRawFd, time::Duration};
unsafe extern "C" {
    fn monogram_verified_stream_new(fd: i32) -> *mut gio::ffi::GInputStream;
    fn monogram_image_open(
        stream: *mut gio::ffi::GInputStream,
        cancel: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut glib::gobject_ffi::GObject;
    fn monogram_image_frame(
        image: *mut glib::gobject_ffi::GObject,
        edge: u32,
        delay: *mut i64,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut gdk::ffi::GdkTexture;
    fn gly_image_get_mime_type(image: *mut glib::gobject_ffi::GObject) -> *const std::ffi::c_char;
    fn gly_image_get_width(image: *mut glib::gobject_ffi::GObject) -> u32;
    fn gly_image_get_height(image: *mut glib::gobject_ffi::GObject) -> u32;
}
pub fn input_stream(file: crate::downloads::VerifiedFile) -> gio::InputStream {
    unsafe { from_glib_full(monogram_verified_stream_new(file.0.into_raw_fd())) }
}
pub struct Decoder {
    image: glib::Object,
    cancel: gio::Cancellable,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}
pub struct Frame {
    pub texture: gdk::Texture,
    pub delay: Option<Duration>,
}
// Upstream errors can contain loader paths and filenames; keep them out of user diagnostics.
fn clear_error(error: *mut glib::ffi::GError) {
    if !error.is_null() {
        unsafe {
            glib::ffi::g_error_free(error);
        }
    }
}
impl Decoder {
    pub fn open(
        input: crate::downloads::VerifiedFile,
        cancel: &gio::Cancellable,
    ) -> Result<Self, &'static str> {
        Self::open_stream(&input_stream(input), cancel)
    }
    pub fn open_bytes(
        bytes: &glib::Bytes,
        cancel: &gio::Cancellable,
    ) -> Result<Self, &'static str> {
        let stream = gio::MemoryInputStream::from_bytes(bytes);
        Self::open_stream(stream.upcast_ref(), cancel)
    }
    fn open_stream(
        stream: &gio::InputStream,
        cancel: &gio::Cancellable,
    ) -> Result<Self, &'static str> {
        if cancel.is_cancelled() {
            return Err("Image preview cancelled");
        }
        let mut error = std::ptr::null_mut();
        let raw = unsafe {
            monogram_image_open(stream.to_glib_none().0, cancel.to_glib_none().0, &mut error)
        };
        clear_error(error);
        if raw.is_null() {
            return Err(if cancel.is_cancelled() {
                "Image preview cancelled"
            } else {
                "Image preview unavailable"
            });
        }
        let image: glib::Object = unsafe { from_glib_full(raw) };
        let mime = unsafe {
            let p = gly_image_get_mime_type(raw);
            if p.is_null() {
                String::new()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        if !mime.starts_with("image/") {
            return Err("Unsupported image format");
        }
        let width = unsafe { gly_image_get_width(raw) };
        let height = unsafe { gly_image_get_height(raw) };
        Ok(Self {
            image,
            cancel: cancel.clone(),
            mime,
            width,
            height,
        })
    }
    pub fn frame(&mut self, edge: u32) -> Result<Frame, &'static str> {
        if self.cancel.is_cancelled() {
            return Err("Image preview cancelled");
        }
        if !(1..=8192).contains(&edge) {
            return Err("Invalid preview dimensions");
        }
        let mut error = std::ptr::null_mut();
        let mut delay = 0;
        let raw = unsafe {
            monogram_image_frame(self.image.to_glib_none().0, edge, &mut delay, &mut error)
        };
        clear_error(error);
        if raw.is_null() {
            return Err(if self.cancel.is_cancelled() {
                "Image preview cancelled"
            } else {
                "Image frame unavailable"
            });
        }
        let texture: gdk::Texture = unsafe { from_glib_full(raw) };
        // Most raster loaders ignore scale requests. Resample decoded, trusted pixels off GTK.
        let texture = fit(texture, edge)?;
        if self.cancel.is_cancelled() {
            return Err("Image preview cancelled");
        }
        Ok(Frame {
            texture,
            delay: (delay > 0).then(|| Duration::from_micros(delay as u64)),
        })
    }
}
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SourceStamp {
    device: u64,
    inode: u64,
    length: u64,
    modified: i64,
    modified_ns: i64,
}
impl SourceStamp {
    fn new(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: metadata.mtime(),
            modified_ns: metadata.mtime_nsec(),
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct PreviewMetadata {
    source: SourceStamp,
    mime: String,
    width: u32,
    height: u32,
    animated: bool,
    checksum: Vec<u8>,
}
fn preview_path(source: &std::path::Path, key: &str, edge: u32) -> Option<std::path::PathBuf> {
    Some(
        source
            .parent()?
            .join("thumbnails")
            .join(format!("{key}-{edge}.preview")),
    )
}
fn cached_preview(
    path: &std::path::Path,
    source: &SourceStamp,
    edge: u32,
    cancel: &gio::Cancellable,
) -> Option<crate::image_ui::Image> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(12 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 12 * 1024 * 1024 {
        return None;
    }
    let split = bytes.iter().take(2048).position(|byte| *byte == b'\n')?;
    let metadata: PreviewMetadata = serde_json::from_slice(&bytes[..split]).ok()?;
    let png = &bytes[split + 1..];
    if &metadata.source != source
        || metadata.width == 0
        || metadata.height == 0
        || !metadata.mime.starts_with("image/")
        || openssl::sha::sha256(png).as_slice() != metadata.checksum
    {
        return None;
    }
    let mut decoder = Decoder::open_bytes(&glib::Bytes::from_owned(png.to_vec()), cancel).ok()?;
    let texture = decoder.frame(edge).ok()?.texture;
    Some(crate::image_ui::Image {
        texture,
        mime: metadata.mime,
        width: metadata.width,
        height: metadata.height,
        animated: metadata.animated,
    })
}
fn save_preview(
    path: &std::path::Path,
    source: SourceStamp,
    image: &crate::image_ui::Image,
    cancel: &gio::Cancellable,
) {
    use std::{
        fs,
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    if cancel.is_cancelled() {
        return;
    }
    let png = image.texture.save_to_png_bytes();
    let metadata = PreviewMetadata {
        source,
        mime: image.mime.clone(),
        width: image.width,
        height: image.height,
        animated: image.animated,
        checksum: openssl::sha::sha256(png.as_ref()).to_vec(),
    };
    let Ok(header) = serde_json::to_vec(&metadata) else {
        return;
    };
    let Some(dir) = path.parent() else { return };
    let _guard = crate::downloads::CACHE_IO.lock().unwrap();
    if cancel.is_cancelled()
        || fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .is_err()
    {
        return;
    }
    let partial = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)?;
        output.write_all(&header)?;
        output.write_all(b"\n")?;
        output.write_all(png.as_ref())?;
        if !cancel.is_cancelled() {
            fs::rename(&partial, path)?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(partial);
    if result.is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()?.to_str()? != "preview" {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| {
                (
                    metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                    metadata.len(),
                    path,
                )
            })
        })
        .collect();
    let mut bytes: u64 = files.iter().map(|(_, size, _)| size).sum();
    let mut count = files.len();
    if bytes <= 128 * 1024 * 1024 && count <= 256 {
        return;
    }
    files.sort_unstable_by_key(|(modified, _, _)| *modified);
    for (_, size, path) in files {
        if bytes <= 128 * 1024 * 1024 && count <= 256 {
            break;
        }
        if fs::remove_file(path).is_ok() {
            bytes = bytes.saturating_sub(size);
            count -= 1;
        }
    }
}
/// Worker-only. Originals remain authenticated; private derived thumbnails avoid full redecodes.
pub fn preview(
    file: &crate::file_share::Attachment,
    path: &std::path::Path,
    edge: u32,
    cancel: &gio::Cancellable,
    stopped: &std::sync::atomic::AtomicBool,
    allow_decode: bool,
) -> Result<crate::image_ui::Image, &'static str> {
    use std::sync::atomic::Ordering;
    let cached = (edge <= 1536)
        .then(|| preview_path(path, &file.key(), edge))
        .flatten();
    if let Some(cached) = cached.as_ref()
        && let Ok(metadata) = path.metadata()
        && let Some(image) = cached_preview(cached, &SourceStamp::new(&metadata), edge, cancel)
    {
        if !stopped.load(Ordering::Acquire) {
            return Ok(image);
        }
    }
    if !allow_decode {
        return Err("Preview not cached");
    }
    let input = crate::downloads::verified_input(path, file, stopped)?;
    let stamp = input
        .0
        .metadata()
        .map(|metadata| SourceStamp::new(&metadata))
        .ok();
    let mut decoder = Decoder::open(input, cancel)?;
    let frame = decoder.frame(edge)?;
    if stopped.load(Ordering::Acquire) {
        return Err("Image preview cancelled");
    }
    let image = crate::image_ui::Image {
        texture: frame.texture,
        mime: decoder.mime,
        width: decoder.width,
        height: decoder.height,
        animated: frame.delay.is_some(),
    };
    if let (Some(path), Some(stamp)) = (cached, stamp) {
        save_preview(&path, stamp, &image, cancel);
    }
    Ok(image)
}
pub fn play<W: IsA<gtk::Widget> + Clone + 'static>(
    picture: &W,
    path: std::path::PathBuf,
    file: crate::file_share::Attachment,
    edge: u32,
    visible: impl Fn(&W) -> bool + 'static,
    draw: impl Fn(&W, gdk::Texture) + 'static,
) {
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        },
        time::Instant,
    };
    static WORKERS: AtomicUsize = AtomicUsize::new(0);
    let picture = picture.downgrade();
    let mut frames = None;
    let mut next = Instant::now();
    glib::timeout_add_local(Duration::from_millis(25), move || {
        let Some(picture) = picture.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !picture.is_mapped() || !visible(&picture) {
            frames = None;
            return glib::ControlFlow::Continue;
        }
        if let Some(rx) = frames.as_ref() {
            if Instant::now() < next {
                return glib::ControlFlow::Continue;
            }
            match mpsc::Receiver::<Frame>::try_recv(rx) {
                Ok(frame) => {
                    draw(&picture, frame.texture);
                    let Some(delay) = frame.delay else {
                        return glib::ControlFlow::Break;
                    };
                    next = Instant::now()
                        + delay.clamp(Duration::from_millis(25), Duration::from_secs(60));
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => return glib::ControlFlow::Break,
            }
        } else if WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 4).then_some(n + 1)
            })
            .is_ok()
        {
            let (tx, rx) = mpsc::sync_channel(1);
            let (path, file) = (path.clone(), file.clone());
            std::thread::spawn(move || {
                let input = crate::downloads::verified_input(&path, &file, &AtomicBool::new(false));
                if let Ok(input) = input
                    && let Ok(mut decoder) = Decoder::open(input, &gio::Cancellable::new())
                {
                    while let Ok(frame) = decoder.frame(edge) {
                        let animated = frame.delay.is_some();
                        if tx.send(frame).is_err() || !animated {
                            break;
                        }
                    }
                }
                WORKERS.fetch_sub(1, Ordering::AcqRel);
            });
            next = Instant::now();
            frames = Some(rx);
        }
        glib::ControlFlow::Continue
    });
}
impl crate::avatar::Avatar {
    /// Run on a blocking worker. The immutable, downscaled texture is safe to hand to GTK.
    pub fn decode(&self, cancel: &gio::Cancellable) -> Result<gdk::Texture, &'static str> {
        let mut image = Decoder::open_bytes(&self.bytes, cancel)?;
        let valid_mime = if self.pep {
            image.mime == "image/png"
        } else {
            matches!(
                image.mime.as_str(),
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            )
        };
        if !valid_mime
            || image.width > 4096
            || image.height > 4096
            || u64::from(image.width) * u64::from(image.height) > 8_000_000
        {
            return Err("Unsupported avatar format or dimensions");
        }
        Ok(image.frame(256)?.texture)
    }
}
fn fit(texture: gdk::Texture, edge: u32) -> Result<gdk::Texture, &'static str> {
    let (w, h) = (texture.width(), texture.height());
    if w <= edge as i32 && h <= edge as i32 {
        return Ok(texture);
    }
    let ratio = edge as f64 / w.max(h) as f64;
    let (nw, nh) = (
        (w as f64 * ratio).round().max(1.) as i32,
        (h as f64 * ratio).round().max(1.) as i32,
    );
    let stride = cairo::Format::ARgb32
        .stride_for_width(w as u32)
        .map_err(|_| "Image layout unavailable")?;
    let mut data = vec![0u8; stride as usize * h as usize];
    texture.download(&mut data, stride as usize);
    let input = cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, w, h, stride)
        .map_err(|_| "Image resize unavailable")?;
    let mut output = cairo::ImageSurface::create(cairo::Format::ARgb32, nw, nh)
        .map_err(|_| "Image resize unavailable")?;
    {
        let context = cairo::Context::new(&output).map_err(|_| "Image resize unavailable")?;
        context.scale(nw as f64 / w as f64, nh as f64 / h as f64);
        context
            .set_source_surface(&input, 0., 0.)
            .map_err(|_| "Image resize unavailable")?;
        context.source().set_filter(cairo::Filter::Bilinear);
        context.paint().map_err(|_| "Image resize unavailable")?;
    }
    output.flush();
    let stride = output.stride() as usize;
    let bytes = glib::Bytes::from_owned(
        output
            .data()
            .map_err(|_| "Image resize unavailable")?
            .to_vec(),
    );
    Ok(gdk::MemoryTexture::new(
        nw,
        nh,
        gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &bytes,
        stride,
    )
    .upcast())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::atomic::AtomicBool};
    #[test]
    #[ignore = "requires real bubblewrap namespaces and glycin loaders outside the Nix build sandbox"]
    fn sandboxed_inline_avatar_decodes_from_memory() {
        let bytes = glib::Bytes::from_owned([64u8, 64, 64, 255].repeat(360 * 180));
        let png = gdk::MemoryTexture::new(360, 180, gdk::MemoryFormat::R8g8b8a8, &bytes, 360 * 4)
            .save_to_png_bytes();
        let avatar = crate::avatar::Avatar::new(png.as_ref().to_vec(), true);
        let cancel = gio::Cancellable::new();
        let texture = avatar.decode(&cancel).unwrap();
        assert_eq!((texture.width(), texture.height()), (256, 128));
        cancel.cancel();
        assert!(avatar.decode(&cancel).is_err());
    }
    fn texture(w: i32, h: i32) -> gdk::Texture {
        let bytes = glib::Bytes::from_owned([0u8, 0, 128, 128].repeat(w as usize * h as usize));
        gdk::MemoryTexture::new(
            w,
            h,
            gdk::MemoryFormat::B8g8r8a8Premultiplied,
            &bytes,
            w as usize * 4,
        )
        .upcast()
    }
    #[test]
    fn downscale_preserves_aspect_and_premultiplied_alpha() {
        let result = fit(texture(256, 128), 64).unwrap();
        assert_eq!((result.width(), result.height()), (64, 32));
        let mut data = vec![0; 64 * 32 * 4];
        result.download(&mut data, 64 * 4);
        assert_eq!(&data[..4], &[0, 0, 128, 128]);
        let small = fit(texture(16, 8), 64).unwrap();
        assert_eq!((small.width(), small.height()), (16, 8));
    }
    #[test]
    #[ignore = "requires real bubblewrap namespaces and glycin loaders outside the Nix build sandbox"]
    fn sandboxed_verified_descriptor_decode_and_animation() {
        assert_eq!(std::env::var("MONOGRAM_IMAGE_FIXTURE").as_deref(), Ok("1"));
        let root = std::env::temp_dir().join(format!("monogram-images-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("image");
        let png = texture(640, 320).save_to_png_bytes();
        fs::write(&path, &png).unwrap();
        let mut expected =
            crate::file_share::Attachment::bare("https://fixture.invalid/image").unwrap();
        expected.size = Some(png.len() as u64);
        expected
            .hashes
            .insert("sha-256".into(), openssl::sha::sha256(&png).to_vec());
        let input =
            crate::downloads::verified_input(&path, &expected, &AtomicBool::new(false)).unwrap();
        fs::rename(&path, root.join("original")).unwrap();
        fs::write(&path, b"a replacement cannot replace the verified inode").unwrap();
        let cancel = gio::Cancellable::new();
        let mut image = Decoder::open(input, &cancel).unwrap();
        assert_eq!(image.mime, "image/png");
        assert_eq!((image.width, image.height), (640, 320));
        let frame = image.frame(80).unwrap();
        assert_eq!((frame.texture.width(), frame.texture.height()), (80, 40));
        assert!(frame.delay.is_none());
        cancel.cancel();
        assert_eq!(image.frame(80).err(), Some("Image preview cancelled"));
        assert!(
            crate::downloads::verified_input(&path, &expected, &AtomicBool::new(false)).is_err()
        );
        let gif = crate::widgets::asset("preview-0.gif");
        let bytes = fs::read(&gif).unwrap();
        expected.size = Some(bytes.len() as u64);
        expected
            .hashes
            .insert("sha-256".into(), openssl::sha::sha256(&bytes).to_vec());
        let input =
            crate::downloads::verified_input(&gif, &expected, &AtomicBool::new(false)).unwrap();
        let mut image = Decoder::open(input, &gio::Cancellable::new()).unwrap();
        assert_eq!(image.mime, "image/gif");
        let first = image.frame(100).unwrap();
        let second = image.frame(100).unwrap();
        assert!(first.delay.is_some() && second.delay.is_some());
        assert!(first.texture.width() <= 100 && first.texture.height() <= 100);
        let bad = root.join("invalid");
        fs::write(&bad, b"not an image").unwrap();
        expected.hashes.clear();
        expected.size = Some(12);
        let input =
            crate::downloads::verified_input(&bad, &expected, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            Decoder::open(input, &gio::Cancellable::new()).err(),
            Some("Image preview unavailable")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
