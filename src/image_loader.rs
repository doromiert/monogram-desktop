//! Decode verified attachment descriptors through libglycin's mandatory Linux sandbox.
//! Construct and advance decoders on a worker; only immutable textures cross to GTK.
use gtk::{cairo, gdk, gio, glib, glib::translate::*, prelude::*};
use std::{ffi::CStr, os::fd::IntoRawFd, time::Duration};
unsafe extern "C" {
    fn monogram_verified_stream_new(fd: i32) -> *mut gio::ffi::GInputStream;
    fn monogram_image_open(
        fd: i32,
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
        if cancel.is_cancelled() {
            return Err("Image preview cancelled");
        }
        let mut error = std::ptr::null_mut();
        // The C adapter owns and closes this verified descriptor, even on failure.
        let raw = unsafe {
            monogram_image_open(input.0.into_raw_fd(), cancel.to_glib_none().0, &mut error)
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
