//! Media authoring plans: what the user asked to do to a photo or video, checked against the
//! limits below before anything touches the file.
//!
//! This module only does arithmetic and validation. It never opens, decodes or encodes media:
//! that happens in a bubblewrap-confined GStreamer helper process, which receives a `Plan` as
//! bounded JSON and reports back the source facts (`SourceInfo`) that this module re-validates,
//! because they come from parsing an untrusted file.
//!
//! Order of operations is fixed: crop (in source coordinates), rotate, then scale down to fit
//! `max_edge`. Video trims are applied to the source timeline.
use serde::{Deserialize, Serialize};
use std::fmt;

/// Hard caps. A request or probe result outside them is refused, not clamped, so a hostile file
/// cannot make the helper allocate or run for an unbounded time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_input_bytes: u64,
    pub max_source_pixels: u64,
    pub max_source_edge: u32,
    pub max_output_edge: u32,
    pub max_source_duration_ms: u64,
    pub min_trim_ms: u64,
    pub max_output_bytes: u64,
    /// Largest accepted job description, in bytes.
    pub max_job_json: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 512 * 1024 * 1024,
            max_source_pixels: 64 * 1024 * 1024,
            max_source_edge: 16384,
            max_output_edge: 4096,
            max_source_duration_ms: 60 * 60 * 1000,
            min_trim_ms: 100,
            max_output_bytes: 256 * 1024 * 1024,
            max_job_json: 16 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Image,
    Video,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    #[default]
    None,
    Clockwise90,
    Half,
    Counterclockwise90,
}
impl Rotation {
    fn swaps_axes(self) -> bool {
        matches!(self, Self::Clockwise90 | Self::Counterclockwise90)
    }
}

/// What the editor asks for. Unknown fields are rejected so a newer or hostile sender cannot
/// smuggle options the helper does not understand.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub crop: Option<Rect>,
    #[serde(default)]
    pub rotation: Rotation,
    /// Keep `[start_ms, end_ms)` of the source timeline. Video only.
    pub trim: Option<(u64, u64)>,
    /// Scale down so the longest edge fits. Never scales up.
    pub max_edge: Option<u32>,
}
impl Job {
    pub fn from_json(bytes: &[u8], limits: &Limits) -> Result<Self, Error> {
        if bytes.len() > limits.max_job_json {
            return Err(Error::JobTooLarge);
        }
        serde_json::from_slice(bytes).map_err(|_| Error::MalformedJob)
    }
}

/// Facts the helper read from the source. Untrusted: validated again here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInfo {
    pub kind: Kind,
    pub width: u32,
    pub height: u32,
    pub duration_ms: Option<u64>,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    JobTooLarge,
    MalformedJob,
    SourceTooLarge,
    SourceDimensions,
    SourceDuration,
    CropOutside,
    CropTooSmall,
    TrimOnImage,
    TrimRange,
    OutputEdge,
    UnsupportedFormat,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::JobTooLarge | Self::MalformedJob => "The edit request is not valid",
            Self::SourceTooLarge => "This file is too large to edit",
            Self::SourceDimensions => "This picture or video has unsupported dimensions",
            Self::SourceDuration => "This video is too long to edit",
            Self::CropOutside => "The crop area is outside the picture",
            Self::CropTooSmall => "The crop area is too small",
            Self::TrimOnImage => "Pictures cannot be trimmed",
            Self::TrimRange => "Choose a longer part of the video to keep",
            Self::OutputEdge => "The requested size is not supported",
            Self::UnsupportedFormat => "Only JPEG, PNG and WebP pictures can be edited",
        })
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Webp,
}

/// How much of a file `sniff_image` needs: enough for a large EXIF block before the JPEG frame
/// header. The helper reads this much and never trusts a decoder to size its own allocations.
pub const SNIFF_BYTES: usize = 1024 * 1024;

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?)))
}
fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}
fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 3)?;
    Some(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
}

/// Identify a picture from its magic bytes and read its pixel size from the header alone, so a
/// tiny file that claims a huge canvas is refused before any decoder allocates for it. Only
/// formats with an explicit decoder in the helper are accepted; everything else is refused.
pub fn sniff_image(head: &[u8]) -> Result<(Format, u32, u32), Error> {
    let bad = Error::UnsupportedFormat;
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        // The first chunk must be IHDR, 13 bytes long.
        if head.get(12..16) != Some(b"IHDR") || be32(head, 8) != Some(13) {
            return Err(bad);
        }
        return Ok((Format::Png, be32(head, 16).ok_or(bad)?, be32(head, 20).ok_or(bad)?));
    }
    if head.starts_with(&[0xff, 0xd8, 0xff]) {
        let mut at = 2;
        // Walk the marker segments up to the first frame header.
        for _ in 0..512 {
            while head.get(at) == Some(&0xff) && head.get(at + 1) == Some(&0xff) {
                at += 1;
            }
            if head.get(at) != Some(&0xff) {
                return Err(bad);
            }
            let marker = *head.get(at + 1).ok_or(bad)?;
            match marker {
                0xd8 | 0x01 | 0xd0..=0xd7 => at += 2,
                // SOFn except DHT (c4), JPG (c8) and DAC (cc): precision, height, width.
                0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                    return Ok((Format::Jpeg, be16(head, at + 7).ok_or(bad)?, be16(head, at + 5).ok_or(bad)?));
                }
                0xd9 | 0xda => return Err(bad),
                _ => {
                    let length = be16(head, at + 2).ok_or(bad)? as usize;
                    if length < 2 {
                        return Err(bad);
                    }
                    at += 2 + length;
                }
            }
        }
        return Err(bad);
    }
    if head.len() >= 30 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        let (width, height) = match &head[12..16] {
            // Lossy: 14-bit sizes after the 3-byte frame tag and start code.
            b"VP8 " if head[23..26] == [0x9d, 0x01, 0x2a] => (
                u32::from(u16::from_le_bytes([head[26], head[27]]) & 0x3fff),
                u32::from(u16::from_le_bytes([head[28], head[29]]) & 0x3fff),
            ),
            // Lossless: signature byte, then 14-bit width-1 and height-1.
            b"VP8L" if head[20] == 0x2f => {
                let bits = u32::from_le_bytes(head[21..25].try_into().unwrap());
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            }
            // Extended: 24-bit canvas width-1 and height-1.
            b"VP8X" => (le24(head, 24).ok_or(bad)? + 1, le24(head, 27).ok_or(bad)? + 1),
            _ => return Err(bad),
        };
        return Ok((Format::Webp, width, height));
    }
    Err(bad)
}

/// A fully resolved, bounds-checked set of operations: everything the helper needs, with no
/// remaining choices that could go out of range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub kind: Kind,
    pub crop: Rect,
    pub rotation: Rotation,
    pub trim: Option<(u64, u64)>,
    pub output_width: u32,
    pub output_height: u32,
    /// The helper must stop and delete its output beyond this size.
    pub max_output_bytes: u64,
}

fn even(value: u32) -> u32 {
    value & !1
}

impl Plan {
    /// Re-check a plan the helper received over a pipe. It was validated by the sender, but the
    /// helper does not trust its caller: every geometry value is range-checked again.
    pub fn check(&self, source: &SourceInfo, limits: &Limits) -> Result<(), Error> {
        let right = self.crop.x.checked_add(self.crop.width).ok_or(Error::CropOutside)?;
        let bottom = self.crop.y.checked_add(self.crop.height).ok_or(Error::CropOutside)?;
        if self.kind != source.kind
            || source.width > limits.max_source_edge
            || source.height > limits.max_source_edge
            || u64::from(source.width) * u64::from(source.height) > limits.max_source_pixels
        {
            return Err(Error::SourceDimensions);
        }
        if right > source.width || bottom > source.height || self.crop.width == 0 || self.crop.height == 0 {
            return Err(Error::CropOutside);
        }
        let valid = |edge: u32| (1..=limits.max_output_edge).contains(&edge);
        if !valid(self.output_width) || !valid(self.output_height) {
            return Err(Error::OutputEdge);
        }
        if self.trim.is_some() && self.kind == Kind::Image {
            return Err(Error::TrimOnImage);
        }
        if self.max_output_bytes == 0 || self.max_output_bytes > limits.max_output_bytes {
            return Err(Error::OutputEdge);
        }
        Ok(())
    }
}

/// Validate `job` against what the helper found in the file and produce the plan to run.
pub fn plan(job: &Job, source: &SourceInfo, limits: &Limits) -> Result<Plan, Error> {
    if source.bytes == 0 || source.bytes > limits.max_input_bytes {
        return Err(Error::SourceTooLarge);
    }
    if source.width == 0
        || source.height == 0
        || source.width > limits.max_source_edge
        || source.height > limits.max_source_edge
        || u64::from(source.width) * u64::from(source.height) > limits.max_source_pixels
    {
        return Err(Error::SourceDimensions);
    }
    let video = source.kind == Kind::Video;
    // H.264/VP9 chroma subsampling needs even dimensions and offsets.
    let min_edge = if video { 2 } else { 1 };

    let mut crop = job.crop.unwrap_or(Rect {
        x: 0,
        y: 0,
        width: source.width,
        height: source.height,
    });
    let right = crop.x.checked_add(crop.width).ok_or(Error::CropOutside)?;
    let bottom = crop.y.checked_add(crop.height).ok_or(Error::CropOutside)?;
    if right > source.width || bottom > source.height {
        return Err(Error::CropOutside);
    }
    if video {
        crop = Rect {
            x: even(crop.x),
            y: even(crop.y),
            width: even(crop.width),
            height: even(crop.height),
        };
    }
    if crop.width < min_edge || crop.height < min_edge {
        return Err(Error::CropTooSmall);
    }

    let trim = match (job.trim, source.kind) {
        (None, _) => None,
        (Some(_), Kind::Image) => return Err(Error::TrimOnImage),
        (Some((start, end)), Kind::Video) => {
            let duration = source.duration_ms.ok_or(Error::SourceDuration)?;
            if duration == 0 || duration > limits.max_source_duration_ms {
                return Err(Error::SourceDuration);
            }
            if end > duration || end.saturating_sub(start) < limits.min_trim_ms || start >= end {
                return Err(Error::TrimRange);
            }
            // Keeping the whole video is not a trim.
            (start != 0 || end != duration).then_some((start, end))
        }
    };
    if video && trim.is_none() {
        let duration = source.duration_ms.ok_or(Error::SourceDuration)?;
        if duration == 0 || duration > limits.max_source_duration_ms {
            return Err(Error::SourceDuration);
        }
    }

    let (mut width, mut height) = if job.rotation.swaps_axes() {
        (crop.height, crop.width)
    } else {
        (crop.width, crop.height)
    };
    let max_edge = match job.max_edge {
        Some(0) => return Err(Error::OutputEdge),
        Some(edge) => edge.min(limits.max_output_edge),
        None => limits.max_output_edge,
    };
    let longest = width.max(height);
    if longest > max_edge {
        // u64 math: the product of two u32 values cannot overflow it.
        let scale = |value: u32| (u64::from(value) * u64::from(max_edge) / u64::from(longest)) as u32;
        width = scale(width);
        height = scale(height);
    }
    if video {
        width = even(width);
        height = even(height);
    }
    if width < min_edge || height < min_edge {
        return Err(Error::OutputEdge);
    }

    Ok(Plan {
        kind: source.kind,
        crop,
        rotation: job.rotation,
        trim,
        output_width: width,
        output_height: height,
        max_output_bytes: limits.max_output_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        max_input_bytes: 512 * 1024 * 1024,
        max_source_pixels: 64 * 1024 * 1024,
        max_source_edge: 16384,
        max_output_edge: 4096,
        max_source_duration_ms: 60 * 60 * 1000,
        min_trim_ms: 100,
        max_output_bytes: 256 * 1024 * 1024,
        max_job_json: 16 * 1024,
    };
    fn image(width: u32, height: u32) -> SourceInfo {
        SourceInfo { kind: Kind::Image, width, height, duration_ms: None, bytes: 1000 }
    }
    fn video(width: u32, height: u32, duration_ms: u64) -> SourceInfo {
        SourceInfo { kind: Kind::Video, width, height, duration_ms: Some(duration_ms), bytes: 1000 }
    }

    #[test]
    fn an_empty_job_keeps_the_picture_and_only_applies_the_size_cap() {
        let plan = plan(&Job::default(), &image(1000, 500), &LIMITS).unwrap();
        assert_eq!((plan.output_width, plan.output_height), (1000, 500));
        assert_eq!(plan.crop, Rect { x: 0, y: 0, width: 1000, height: 500 });
        let big = super::plan(&Job::default(), &image(8000, 4000), &LIMITS).unwrap();
        assert_eq!((big.output_width, big.output_height), (4096, 2048), "scaled down, aspect kept");
        let small = super::plan(
            &Job { max_edge: Some(10_000), ..Default::default() },
            &image(100, 50),
            &LIMITS,
        )
        .unwrap();
        assert_eq!((small.output_width, small.output_height), (100, 50), "never scaled up");
    }

    #[test]
    fn crops_must_stay_inside_the_source_even_when_arithmetic_would_overflow() {
        let crop = |x, y, width, height| Job { crop: Some(Rect { x, y, width, height }), ..Default::default() };
        let source = image(100, 100);
        assert!(plan(&crop(10, 10, 90, 90), &source, &LIMITS).is_ok());
        assert_eq!(plan(&crop(10, 10, 91, 90), &source, &LIMITS), Err(Error::CropOutside));
        assert_eq!(plan(&crop(u32::MAX, 0, 2, 10), &source, &LIMITS), Err(Error::CropOutside));
        assert_eq!(plan(&crop(0, 0, 0, 10), &source, &LIMITS), Err(Error::CropTooSmall));
    }

    #[test]
    fn rotation_swaps_the_output_axes() {
        let job = Job {
            crop: Some(Rect { x: 0, y: 0, width: 200, height: 100 }),
            rotation: Rotation::Clockwise90,
            ..Default::default()
        };
        let plan = plan(&job, &image(400, 400), &LIMITS).unwrap();
        assert_eq!((plan.output_width, plan.output_height), (100, 200));
    }

    #[test]
    fn video_geometry_is_made_even_and_refuses_unusable_sizes() {
        let job = Job {
            crop: Some(Rect { x: 11, y: 5, width: 101, height: 51 }),
            ..Default::default()
        };
        let plan = plan(&job, &video(640, 360, 5000), &LIMITS).unwrap();
        assert_eq!(plan.crop, Rect { x: 10, y: 4, width: 100, height: 50 });
        assert_eq!((plan.output_width % 2, plan.output_height % 2), (0, 0));
        let sliver = Job { crop: Some(Rect { x: 0, y: 0, width: 1, height: 100 }), ..Default::default() };
        assert_eq!(super::plan(&sliver, &video(640, 360, 5000), &LIMITS), Err(Error::CropTooSmall));
    }

    #[test]
    fn trims_are_ordered_inside_the_video_and_not_applied_to_pictures() {
        let trim = |range| Job { trim: Some(range), ..Default::default() };
        let source = video(640, 360, 10_000);
        assert_eq!(plan(&trim((1000, 4000)), &source, &LIMITS).unwrap().trim, Some((1000, 4000)));
        assert_eq!(plan(&trim((0, 10_000)), &source, &LIMITS).unwrap().trim, None, "whole video is no trim");
        for bad in [(4000, 1000), (1000, 1000), (1000, 1050), (0, 10_001), (u64::MAX, 0)] {
            assert_eq!(plan(&trim(bad), &source, &LIMITS), Err(Error::TrimRange), "{bad:?}");
        }
        assert_eq!(plan(&trim((0, 1000)), &image(10, 10), &LIMITS), Err(Error::TrimOnImage));
    }

    #[test]
    fn hostile_probe_results_are_refused() {
        let job = Job::default();
        assert_eq!(plan(&job, &image(0, 10), &LIMITS), Err(Error::SourceDimensions));
        assert_eq!(plan(&job, &image(16385, 10), &LIMITS), Err(Error::SourceDimensions));
        assert_eq!(plan(&job, &image(10_000, 10_000), &LIMITS), Err(Error::SourceDimensions), "pixel budget");
        let mut huge = image(10, 10);
        huge.bytes = LIMITS.max_input_bytes + 1;
        assert_eq!(plan(&job, &huge, &LIMITS), Err(Error::SourceTooLarge));
        huge.bytes = 0;
        assert_eq!(plan(&job, &huge, &LIMITS), Err(Error::SourceTooLarge));
        assert_eq!(plan(&job, &video(10, 10, 0), &LIMITS), Err(Error::SourceDuration));
        assert_eq!(
            plan(&job, &video(10, 10, LIMITS.max_source_duration_ms + 1), &LIMITS),
            Err(Error::SourceDuration)
        );
        let mut unknown = video(10, 10, 1000);
        unknown.duration_ms = None;
        assert_eq!(plan(&job, &unknown, &LIMITS), Err(Error::SourceDuration));
    }

    #[test]
    fn the_output_edge_request_is_clamped_to_the_limit_and_zero_is_refused() {
        let wide = |edge| Job { max_edge: Some(edge), ..Default::default() };
        let plan_wide = plan(&wide(u32::MAX), &image(8000, 100), &LIMITS).unwrap();
        assert_eq!(plan_wide.output_width, 4096);
        assert_eq!(plan(&wide(0), &image(10, 10), &LIMITS), Err(Error::OutputEdge));
        // Scaling a 1-pixel-tall strip would round its height to zero.
        assert_eq!(plan(&wide(100), &image(10_000, 10), &LIMITS), Err(Error::OutputEdge));
    }

    #[test]
    fn job_json_is_bounded_and_rejects_unknown_fields() {
        let ok = br#"{"crop":{"x":1,"y":2,"width":30,"height":40},"rotation":"half","max_edge":1280}"#;
        let job = Job::from_json(ok, &LIMITS).unwrap();
        assert_eq!(job.rotation, Rotation::Half);
        assert_eq!(job.max_edge, Some(1280));
        assert_eq!(Job::from_json(br#"{"command":"rm -rf"}"#, &LIMITS), Err(Error::MalformedJob));
        assert_eq!(Job::from_json(br#"{"crop":{"x":-1}}"#, &LIMITS), Err(Error::MalformedJob));
        assert_eq!(Job::from_json(b"not json", &LIMITS), Err(Error::MalformedJob));
        let padding = format!(r#"{{"max_edge":1,"pad":"{}"}}"#, "x".repeat(LIMITS.max_job_json));
        assert_eq!(Job::from_json(padding.as_bytes(), &LIMITS), Err(Error::JobTooLarge));
    }

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes
    }

    #[test]
    fn sniffing_reads_png_jpeg_and_webp_sizes_from_headers_only() {
        assert_eq!(sniff_image(&png_header(640, 480)), Ok((Format::Png, 640, 480)));
        // JPEG: SOI, an APP1 (EXIF-like) block that must be skipped, then SOF0 (h=300, w=500).
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1, 0x00, 0x08, 1, 2, 3, 4, 5, 6];
        jpeg.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 8, 0x01, 0x2c, 0x01, 0xf4, 3]);
        assert_eq!(sniff_image(&jpeg), Ok((Format::Jpeg, 500, 300)));
        let riff = |chunk: &[u8; 4], payload: &[u8]| {
            let mut bytes = b"RIFF\0\0\0\0WEBP".to_vec();
            bytes.extend_from_slice(chunk);
            bytes.extend_from_slice(&[0; 4]);
            bytes.extend_from_slice(payload);
            bytes.resize(bytes.len().max(40), 0);
            bytes
        };
        let mut extended = vec![0; 4];
        extended.extend_from_slice(&[99, 0, 0, 49, 0, 0]); // 100 x 50
        assert_eq!(sniff_image(&riff(b"VP8X", &extended)), Ok((Format::Webp, 100, 50)));
        let mut lossy = vec![0, 0, 0, 0x9d, 0x01, 0x2a];
        lossy.extend_from_slice(&[0x40, 0x01, 0xc8, 0x00]); // 320 x 200
        assert_eq!(sniff_image(&riff(b"VP8 ", &lossy)), Ok((Format::Webp, 320, 200)));
        let lossless = [0x2f, 0x3f, 0x80, 0x00, 0x00]; // (63+1) wide, (2+1) tall
        assert_eq!(sniff_image(&riff(b"VP8L", &lossless)), Ok((Format::Webp, 64, 3)));
    }

    #[test]
    fn sniffing_refuses_other_formats_and_malformed_headers() {
        for head in [
            &b""[..],
            b"GIF89a\x01\x00\x01\x00",
            b"<svg xmlns='http://www.w3.org/2000/svg'/>",
            b"\x00\x00\x00\x18ftypmp42",
            &png_header(1, 1)[..20],
            b"\xff\xd8\xff",
        ] {
            assert_eq!(sniff_image(head), Err(Error::UnsupportedFormat), "{head:?}");
        }
        // The first PNG chunk must be IHDR.
        let mut wrong = png_header(10, 10);
        wrong[12..16].copy_from_slice(b"IDAT");
        assert_eq!(sniff_image(&wrong), Err(Error::UnsupportedFormat));
        // A JPEG whose segment length points past the data, or loops, is refused, not followed.
        let truncated = [0xff, 0xd8, 0xff, 0xe0, 0xff, 0xff, 0, 0];
        assert_eq!(sniff_image(&truncated), Err(Error::UnsupportedFormat));
        let mut endless = vec![0xff, 0xd8];
        for _ in 0..600 {
            endless.extend_from_slice(&[0xff, 0xe0, 0x00, 0x02]);
        }
        assert_eq!(sniff_image(&endless), Err(Error::UnsupportedFormat));
    }

    #[test]
    fn a_tiny_file_claiming_a_huge_canvas_is_refused_by_the_plan() {
        let (_, width, height) = sniff_image(&png_header(60_000, 60_000)).unwrap();
        let source = SourceInfo { kind: Kind::Image, width, height, duration_ms: None, bytes: 100 };
        assert_eq!(plan(&Job::default(), &source, &LIMITS), Err(Error::SourceDimensions));
    }

    #[test]
    fn the_helper_rechecks_plans_it_receives() {
        let source = image(400, 300);
        let good = plan(&Job::default(), &source, &LIMITS).unwrap();
        assert_eq!(good.check(&source, &LIMITS), Ok(()));
        let mut outside = good.clone();
        outside.crop.width = 401;
        assert_eq!(outside.check(&source, &LIMITS), Err(Error::CropOutside));
        let mut overflow = good.clone();
        overflow.crop.x = u32::MAX;
        assert_eq!(overflow.check(&source, &LIMITS), Err(Error::CropOutside));
        let mut huge = good.clone();
        huge.output_width = LIMITS.max_output_edge + 1;
        assert_eq!(huge.check(&source, &LIMITS), Err(Error::OutputEdge));
        let mut zero = good.clone();
        zero.output_height = 0;
        assert_eq!(zero.check(&source, &LIMITS), Err(Error::OutputEdge));
        let mut unbounded = good.clone();
        unbounded.max_output_bytes = u64::MAX;
        assert_eq!(unbounded.check(&source, &LIMITS), Err(Error::OutputEdge));
        let mut video_plan = good.clone();
        video_plan.kind = Kind::Video;
        assert_eq!(video_plan.check(&source, &LIMITS), Err(Error::SourceDimensions));
        assert_eq!(good.check(&image(20_000, 10), &LIMITS), Err(Error::SourceDimensions));
    }

    #[test]
    fn plans_round_trip_through_json_for_the_helper() {
        let plan = plan(
            &Job { rotation: Rotation::Counterclockwise90, trim: Some((500, 3000)), ..Default::default() },
            &video(1280, 720, 5000),
            &LIMITS,
        )
        .unwrap();
        let json = serde_json::to_vec(&plan).unwrap();
        assert!(json.len() < LIMITS.max_job_json);
        assert_eq!(serde_json::from_slice::<Plan>(&json).unwrap(), plan);
    }
}
