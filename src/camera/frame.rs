//! One camera picture: tightly packed 8-bit RGBA, with its size checked.
//!
//! The bytes come from a device through GStreamer, so nothing about them is taken on trust:
//! the size is bounded, the arithmetic is checked, and a frame whose length does not match its
//! size is refused instead of being read past its end.
use gtk::{gdk, gdk_pixbuf, glib};

/// The largest picture kept. The pipeline scales anything bigger down to fit.
pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    Empty,
    TooLarge,
    WrongLength,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Frame {
    /// `data` must be exactly `width * height * 4` bytes of RGBA.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Result<Self, FrameError> {
        if width == 0 || height == 0 {
            return Err(FrameError::Empty);
        }
        if width > MAX_WIDTH || height > MAX_HEIGHT {
            return Err(FrameError::TooLarge);
        }
        let expected = (width as usize).checked_mul(height as usize).and_then(|pixels| pixels.checked_mul(4));
        if expected != Some(data.len()) {
            return Err(FrameError::WrongLength);
        }
        Ok(Self { width, height, data })
    }
    #[cfg(test)]
    pub fn width(&self) -> u32 {
        self.width
    }
    #[cfg(test)]
    pub fn height(&self) -> u32 {
        self.height
    }
    #[cfg(test)]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    /// For showing on screen.
    pub fn texture(&self) -> gdk::MemoryTexture {
        gdk::MemoryTexture::new(
            self.width as i32,
            self.height as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from(&self.data),
            self.width as usize * 4,
        )
    }
    /// A JPEG of the picture. The alpha channel (always opaque from a camera) is dropped here
    /// rather than left to the encoder: given four channels it averages them into grey.
    pub fn to_jpeg(&self, quality: u8) -> Result<Vec<u8>, glib::Error> {
        let rgb: Vec<u8> = self.data.chunks_exact(4).flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect();
        let pixbuf = gdk_pixbuf::Pixbuf::from_bytes(
            &glib::Bytes::from_owned(rgb),
            gdk_pixbuf::Colorspace::Rgb,
            false,
            8,
            self.width as i32,
            self.height as i32,
            self.width as i32 * 3,
        );
        pixbuf.save_to_bufferv("jpeg", &[("quality", &quality.clamp(1, 100).to_string())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::prelude::*;

    fn pixels(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter().copied().cycle().take(width as usize * height as usize * 4).collect()
    }

    #[test]
    fn a_frame_must_have_a_size_within_bounds_and_exactly_that_many_bytes() {
        assert!(Frame::new(2, 2, pixels(2, 2, [1, 2, 3, 255])).is_ok());
        assert!(Frame::new(MAX_WIDTH, MAX_HEIGHT, pixels(MAX_WIDTH, MAX_HEIGHT, [0; 4])).is_ok());
        assert_eq!(Frame::new(0, 4, vec![]), Err(FrameError::Empty));
        assert_eq!(Frame::new(4, 0, vec![]), Err(FrameError::Empty));
        assert_eq!(Frame::new(MAX_WIDTH + 1, 1, vec![0; (MAX_WIDTH as usize + 1) * 4]), Err(FrameError::TooLarge));
        assert_eq!(Frame::new(1, MAX_HEIGHT + 1, vec![0; (MAX_HEIGHT as usize + 1) * 4]), Err(FrameError::TooLarge));
        assert_eq!(Frame::new(u32::MAX, u32::MAX, vec![]), Err(FrameError::TooLarge), "no overflow on absurd sizes");
        for length in [0, 1, 15, 17, 100] {
            assert_eq!(Frame::new(2, 2, vec![0; length]), Err(FrameError::WrongLength), "{length}");
        }
        // Too many bytes is wrong as well as too few: nothing is silently ignored.
        assert_eq!(Frame::new(2, 2, vec![0; 20]), Err(FrameError::WrongLength));
    }

    #[test]
    fn a_frame_keeps_its_size_and_bytes() {
        let data = pixels(3, 2, [10, 20, 30, 255]);
        let frame = Frame::new(3, 2, data.clone()).unwrap();
        assert_eq!((frame.width(), frame.height()), (3, 2));
        assert_eq!(frame.data(), data.as_slice());
    }

    #[test]
    fn the_texture_has_the_frames_size_and_colours() {
        let frame = Frame::new(4, 3, pixels(4, 3, [200, 100, 50, 255])).unwrap();
        let texture = frame.texture();
        assert_eq!((texture.width(), texture.height()), (4, 3));
        let mut back = vec![0u8; 4 * 3 * 4];
        texture.download(&mut back, 4 * 4);
        // GTK hands pixels back as BGRA, so red and blue swap places.
        assert_eq!(&back[..4], &[50, 100, 200, 255]);
        assert_eq!(&back[back.len() - 4..], &[50, 100, 200, 255]);
    }

    #[test]
    fn a_jpeg_decodes_back_to_the_same_size_and_the_same_colours() {
        use gdk_pixbuf::prelude::*;
        // Colours with unequal channels: averaging them into grey (an encoder bug this guards
        // against) would turn red and green into the same 100.
        for rgba in [[220u8, 40, 40, 255], [40, 220, 40, 255], [40, 40, 220, 255], [250, 200, 20, 255], [0, 0, 0, 255], [255, 255, 255, 255]] {
            let frame = Frame::new(64, 48, pixels(64, 48, rgba)).unwrap();
            let jpeg = frame.to_jpeg(90).unwrap();
            assert_eq!(&jpeg[..3], &[0xFF, 0xD8, 0xFF], "a JPEG starts with its marker");
            let loader = gdk_pixbuf::PixbufLoader::new();
            loader.write(&jpeg).unwrap();
            loader.close().unwrap();
            let decoded = loader.pixbuf().unwrap();
            assert_eq!((decoded.width(), decoded.height()), (64, 48));
            let pixel = decoded.read_pixel_bytes();
            for (channel, want) in rgba.iter().take(3).enumerate() {
                let got = pixel[channel];
                assert!((i32::from(got) - i32::from(*want)).abs() < 14, "{rgba:?}: channel {channel} is {got}");
            }
        }
        let frame = Frame::new(64, 48, pixels(64, 48, [220, 40, 40, 255])).unwrap();
        assert!(frame.to_jpeg(90).unwrap().len() < frame.data().len() / 4, "much smaller than the raw picture");
    }

    #[test]
    fn the_alpha_channel_never_changes_the_picture() {
        use gdk_pixbuf::prelude::*;
        let opaque = Frame::new(8, 8, pixels(8, 8, [10, 200, 90, 255])).unwrap().to_jpeg(95).unwrap();
        let clear = Frame::new(8, 8, pixels(8, 8, [10, 200, 90, 0])).unwrap().to_jpeg(95).unwrap();
        assert_eq!(opaque, clear, "alpha is dropped, not blended or averaged");
        let loader = gdk_pixbuf::PixbufLoader::new();
        loader.write(&clear).unwrap();
        loader.close().unwrap();
        assert!(!loader.pixbuf().unwrap().has_alpha());
    }

    #[test]
    fn the_jpeg_quality_is_kept_in_range_and_lower_quality_is_smaller() {
        let noisy: Vec<u8> = (0..64 * 48 * 4).map(|i| ((i * 2654435761usize) >> 7) as u8).collect();
        let frame = Frame::new(64, 48, noisy).unwrap();
        let (low, high) = (frame.to_jpeg(10).unwrap().len(), frame.to_jpeg(95).unwrap().len());
        assert!(low < high, "{low} vs {high}");
        for odd in [0u8, 1, 100, 255] {
            assert!(frame.to_jpeg(odd).is_ok(), "{odd}");
        }
    }
}
