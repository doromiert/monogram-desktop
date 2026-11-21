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
    /// Most marks (drawn shapes) one picture may carry.
    pub max_marks: usize,
    /// Thickest mark outline, in source pixels.
    pub max_stroke: u32,
    /// Most path points one freehand mark may have, and all marks together.
    pub max_path_points: usize,
    pub max_points: usize,
    /// Most pixels in one text mark's bitmap, and in all of them together.
    pub max_text_pixels: u64,
    pub max_text_total: u64,
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
            max_job_json: 1024 * 1024,
            max_marks: 32,
            max_stroke: 128,
            max_path_points: 256,
            max_points: 1024,
            max_text_pixels: 4_000_000,
            max_text_total: 8_000_000,
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

/// Which outline a rectangle-based mark has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Square,
    /// The ellipse that just fits inside the rectangle: a circle when the rectangle is square.
    Circle,
}

/// A position in the picture, `(x, y)` in pixels from its top-left corner. A point may sit on the
/// far edge (`x == width`), so a stroke can reach the border.
pub type Point = (u32, u32);

/// A shape drawn on the picture. Marks are in *source* pixels (the upright picture, before crop
/// and rotation), so a crop that leaves a mark out simply drops it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "shape", rename_all = "snake_case")]
pub enum Mark {
    /// The outline of a rectangle, `stroke` pixels thick, drawn inward from `rect`'s edges.
    Square { rect: Rect, color: [u8; 3], stroke: u32 },
    /// The outline of the ellipse inscribed in `rect`, `stroke` pixels thick, drawn inward.
    Circle { rect: Rect, color: [u8; 3], stroke: u32 },
    /// A line from `from` to `to` with a head at `to`.
    Arrow { from: Point, to: Point, color: [u8; 3], stroke: u32 },
    /// Text the editor already drew into a bitmap, so the sandbox needs no fonts. `mask` is the
    /// coverage of each pixel of `rect` (0 to 255, row by row), deflated and base64 encoded;
    /// the text is painted in `color` where the mask says.
    Text { rect: Rect, color: [u8; 3], mask: String },
    /// A line through `points` in order, with round ends. `pressure` is how hard a pen pressed at
    /// each point, 1 to 255, and the line is that fraction of `stroke` wide there. Empty means the
    /// same width all along (a mouse); otherwise there is exactly one value per point.
    Freehand {
        points: Vec<Point>,
        color: [u8; 3],
        stroke: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pressure: Vec<u8>,
    },
}
impl Mark {
    pub fn of(shape: Shape, rect: Rect, color: [u8; 3], stroke: u32) -> Self {
        match shape {
            Shape::Square => Self::Square { rect, color, stroke },
            Shape::Circle => Self::Circle { rect, color, stroke },
        }
    }
    fn stroke(&self) -> u32 {
        match self {
            Self::Square { stroke, .. }
            | Self::Circle { stroke, .. }
            | Self::Arrow { stroke, .. }
            | Self::Freehand { stroke, .. } => *stroke,
            Self::Text { .. } => 1,
        }
    }
    fn color(&self) -> [u8; 3] {
        match self {
            Self::Square { color, .. }
            | Self::Circle { color, .. }
            | Self::Arrow { color, .. }
            | Self::Freehand { color, .. }
            | Self::Text { color, .. } => *color,
        }
    }
    /// How many path points the mark carries (what `Limits::max_points` counts).
    fn points(&self) -> usize {
        match self {
            Self::Arrow { .. } => 2,
            Self::Freehand { points, .. } => points.len(),
            _ => 0,
        }
    }
}

/// Pack a coverage bitmap (one byte per pixel) for a text mark.
pub fn encode_mask(alpha: &[u8]) -> String {
    use base64::Engine;
    use std::io::Write;
    let mut deflater = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = deflater.write_all(alpha);
    base64::engine::general_purpose::STANDARD.encode(deflater.finish().unwrap_or_default())
}

/// Unpack a text mark's bitmap. `None` unless it holds exactly `width * height` bytes: the
/// inflater is stopped one byte past that, so a hostile mask cannot grow memory.
pub fn decode_mask(mask: &str, width: u32, height: u32) -> Option<Vec<u8>> {
    use base64::Engine;
    use std::io::Read;
    let size = u64::from(width).checked_mul(u64::from(height))?;
    // Deflate cannot be smaller than a thousandth of its output for text, but may be larger than it
    // for noise; allow a generous bound on the encoded form.
    if size == 0 || mask.len() as u64 > size.saturating_mul(2).saturating_add(1024) {
        return None;
    }
    let packed = base64::engine::general_purpose::STANDARD.decode(mask).ok()?;
    let mut alpha = Vec::new();
    flate2::read::ZlibDecoder::new(&packed[..]).take(size + 1).read_to_end(&mut alpha).ok()?;
    (alpha.len() as u64 == size).then_some(alpha)
}

/// Check `marks` against the picture they will be drawn on.
fn check_marks(marks: &[Mark], width: u32, height: u32, limits: &Limits) -> Result<(), Error> {
    if marks.len() > limits.max_marks {
        return Err(Error::TooManyMarks);
    }
    let inside = |(x, y): Point| x <= width && y <= height;
    let mut points = 0usize;
    let mut text_pixels = 0u64;
    for mark in marks {
        match mark {
            Mark::Square { rect, .. } | Mark::Circle { rect, .. } => {
                let right = rect.x.checked_add(rect.width).ok_or(Error::MarkOutside)?;
                let bottom = rect.y.checked_add(rect.height).ok_or(Error::MarkOutside)?;
                if rect.width == 0 || rect.height == 0 || right > width || bottom > height {
                    return Err(Error::MarkOutside);
                }
            }
            Mark::Arrow { from, to, .. } => {
                if !inside(*from) || !inside(*to) || from == to {
                    return Err(Error::MarkOutside);
                }
            }
            Mark::Text { rect, mask, .. } => {
                let right = rect.x.checked_add(rect.width).ok_or(Error::MarkOutside)?;
                let bottom = rect.y.checked_add(rect.height).ok_or(Error::MarkOutside)?;
                if rect.width == 0 || rect.height == 0 || right > width || bottom > height {
                    return Err(Error::MarkOutside);
                }
                let pixels = u64::from(rect.width) * u64::from(rect.height);
                text_pixels += pixels;
                if pixels > limits.max_text_pixels || text_pixels > limits.max_text_total {
                    return Err(Error::MarkText);
                }
                if decode_mask(mask, rect.width, rect.height).is_none() {
                    return Err(Error::MarkText);
                }
                continue;
            }
            Mark::Freehand { points: path, pressure, .. } => {
                if path.is_empty() || path.len() > limits.max_path_points || !path.iter().all(|point| inside(*point)) {
                    return Err(Error::MarkOutside);
                }
                if !pressure.is_empty() && (pressure.len() != path.len() || pressure.contains(&0)) {
                    return Err(Error::MarkPressure);
                }
            }
        }
        points += mark.points();
        if points > limits.max_points {
            return Err(Error::TooManyMarks);
        }
        if !(1..=limits.max_stroke).contains(&mark.stroke()) {
            return Err(Error::MarkStroke);
        }
    }
    Ok(())
}

/// Distance from `(px, py)` to the segment `a`-`b`.
fn segment_distance(px: f64, py: f64, a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length_squared = dx * dx + dy * dy;
    let along = if length_squared == 0. { 0. } else { (((px - a.0) * dx + (py - a.1) * dy) / length_squared).clamp(0., 1.) };
    ((px - (a.0 + along * dx)).powi(2) + (py - (a.1 + along * dy)).powi(2)).sqrt()
}

/// Distance from `(px, py)` to the segment `a`-`b`, and how far along it the nearest point is (0 at
/// `a`, 1 at `b`).
fn segment_nearest(px: f64, py: f64, a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length_squared = dx * dx + dy * dy;
    let along = if length_squared == 0. { 0. } else { (((px - a.0) * dx + (py - a.1) * dy) / length_squared).clamp(0., 1.) };
    (((px - (a.0 + along * dx)).powi(2) + (py - (a.1 + along * dy)).powi(2)).sqrt(), along)
}

/// The line segments that make up an arrow: the shaft and the two sides of the head.
pub fn arrow_segments(from: Point, to: Point, stroke: u32) -> [((f64, f64), (f64, f64)); 3] {
    let (a, b) = ((f64::from(from.0), f64::from(from.1)), (f64::from(to.0), f64::from(to.1)));
    let length = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt().max(1.);
    let head = (f64::from(stroke) * 5.).max(12.).min(length);
    let back = (a.1 - b.1).atan2(a.0 - b.0);
    let side = |turn: f64| (b, (b.0 + head * (back + turn).cos(), b.1 + head * (back + turn).sin()));
    let spread = 28f64.to_radians();
    [(a, b), side(spread), side(-spread)]
}

/// Draw `marks` into a tightly packed RGBA picture of `width` x `height`. Returns false, drawing
/// nothing, when the buffer is not exactly that size. Everything is clipped to the picture.
pub fn paint(rgba: &mut [u8], width: u32, height: u32, marks: &[Mark]) -> bool {
    let (w, h) = (width as usize, height as usize);
    if w.checked_mul(h).and_then(|pixels| pixels.checked_mul(4)) != Some(rgba.len()) {
        return false;
    }
    let put = |rgba: &mut [u8], x: usize, y: usize, color: [u8; 3]| {
        let at = (y * w + x) * 4;
        rgba[at..at + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
    };
    // Every pixel within half the stroke of any of `segments`, scanning only their bounding boxes.
    let stroke_segments = |rgba: &mut [u8], segments: &[((f64, f64), (f64, f64))], thickness: f64, color: [u8; 3]| {
        let reach = thickness / 2.;
        for (a, b) in segments {
            let x0 = (a.0.min(b.0) - reach - 1.).floor().max(0.) as usize;
            let y0 = (a.1.min(b.1) - reach - 1.).floor().max(0.) as usize;
            let x1 = ((a.0.max(b.0) + reach + 1.).ceil().max(0.) as usize).min(w);
            let y1 = ((a.1.max(b.1) + reach + 1.).ceil().max(0.) as usize).min(h);
            for y in y0..y1 {
                for x in x0..x1 {
                    if segment_distance(x as f64 + 0.5, y as f64 + 0.5, *a, *b) <= reach {
                        put(rgba, x, y, color);
                    }
                }
            }
        }
    };
    for mark in marks {
        let (color, thickness) = (mark.color(), mark.stroke());
        match mark {
            Mark::Square { rect, .. } | Mark::Circle { rect, .. } => {
                let circle = matches!(mark, Mark::Circle { .. });
                let (left, top) = (rect.x as usize, rect.y as usize);
                let (right, bottom) = ((left + rect.width as usize).min(w), (top + rect.height as usize).min(h));
                let stroke = (thickness as usize).min(rect.width as usize).min(rect.height as usize);
                // Half the rectangle's size and its centre, for the ellipse test.
                let (rx, ry) = (f64::from(rect.width) / 2., f64::from(rect.height) / 2.);
                let (cx, cy) = (f64::from(rect.x) + rx, f64::from(rect.y) + ry);
                let (inner_x, inner_y) = (rx - stroke as f64, ry - stroke as f64);
                for y in top..bottom {
                    let edge_row = y - top < stroke || bottom.saturating_sub(y) <= stroke;
                    for x in left..right {
                        let on = if circle {
                            let (dx, dy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
                            let inside = (dx / rx).powi(2) + (dy / ry).powi(2) <= 1.;
                            let hollow = inner_x > 0. && inner_y > 0. && (dx / inner_x).powi(2) + (dy / inner_y).powi(2) < 1.;
                            inside && !hollow
                        } else {
                            edge_row || x - left < stroke || right.saturating_sub(x) <= stroke
                        };
                        if on {
                            put(rgba, x, y, color);
                        }
                    }
                }
            }
            Mark::Arrow { from, to, .. } => {
                stroke_segments(rgba, &arrow_segments(*from, *to, thickness), f64::from(thickness), color);
            }
            Mark::Text { rect, mask, .. } => {
                let Some(coverage) = decode_mask(mask, rect.width, rect.height) else {
                    return false;
                };
                for row in 0..rect.height as usize {
                    for column in 0..rect.width as usize {
                        let (x, y) = (rect.x as usize + column, rect.y as usize + row);
                        let level = u32::from(coverage[row * rect.width as usize + column]);
                        if level == 0 || x >= w || y >= h {
                            continue;
                        }
                        let at = (y * w + x) * 4;
                        // Source-over: the text colour over what is already there, keeping any
                        // transparency of the picture where the text is partly covering.
                        let under = u32::from(rgba[at + 3]);
                        let out = level * 255 + under * (255 - level);
                        for channel in 0..3 {
                            let mixed = u32::from(color[channel]) * level * 255 + u32::from(rgba[at + channel]) * under * (255 - level);
                            rgba[at + channel] = (mixed / out.max(1)).min(255) as u8;
                        }
                        rgba[at + 3] = (out / 255).min(255) as u8;
                    }
                }
            }
            Mark::Freehand { points, pressure, .. } => {
                let at = |point: &Point| (f64::from(point.0), f64::from(point.1));
                let half = f64::from(thickness) / 2.;
                // Half the line's width at point `i`: full for no pressure data, else its fraction,
                // never under half a pixel so the line cannot vanish.
                let radius = |i: usize| match pressure.get(i) {
                    Some(level) => (half * f64::from(*level) / 255.).max(0.5),
                    None => half,
                };
                let spans: Vec<_> = match points.as_slice() {
                    [only] => vec![(at(only), at(only), radius(0), radius(0))],
                    many => many.windows(2).enumerate().map(|(i, pair)| (at(&pair[0]), at(&pair[1]), radius(i), radius(i + 1))).collect(),
                };
                for (a, b, ra, rb) in spans {
                    let reach = ra.max(rb);
                    let x0 = (a.0.min(b.0) - reach - 1.).floor().max(0.) as usize;
                    let y0 = (a.1.min(b.1) - reach - 1.).floor().max(0.) as usize;
                    let x1 = ((a.0.max(b.0) + reach + 1.).ceil().max(0.) as usize).min(w);
                    let y1 = ((a.1.max(b.1) + reach + 1.).ceil().max(0.) as usize).min(h);
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let (distance, along) = segment_nearest(x as f64 + 0.5, y as f64 + 0.5, a, b);
                            if distance <= ra + (rb - ra) * along {
                                put(rgba, x, y, color);
                            }
                        }
                    }
                }
            }
        }
    }
    true
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
    /// Shapes to draw, in source pixels. Pictures only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<Mark>,
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
    TooManyMarks,
    MarkOutside,
    MarkStroke,
    MarkPressure,
    MarkText,
    MarksOnVideo,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::JobTooLarge => "There is too much drawn or typed on this picture to save",
            Self::MalformedJob => "The edit request is not valid",
            Self::SourceTooLarge => "This file is too large to edit",
            Self::SourceDimensions => "This picture or video has unsupported dimensions",
            Self::SourceDuration => "This video is too long to edit",
            Self::CropOutside => "The crop area is outside the picture",
            Self::CropTooSmall => "The crop area is too small",
            Self::TrimOnImage => "Pictures cannot be trimmed",
            Self::TrimRange => "Choose a longer part of the video to keep",
            Self::OutputEdge => "The requested size is not supported",
            Self::UnsupportedFormat => "Only JPEG, PNG and WebP pictures can be edited",
            Self::TooManyMarks => "There are too many marks on this picture",
            Self::MarkOutside => "A mark is outside the picture",
            Self::MarkStroke => "A mark's line thickness is not supported",
            Self::MarkText => "A text mark is too large or damaged",
            Self::MarkPressure => "A drawn line's pressure values do not match its points",
            Self::MarksOnVideo => "Videos cannot be drawn on",
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
    /// Drawn first, in source pixels, so crop and rotation carry them along. Left out when empty so
    /// a plain crop or rotation still reaches a helper built before marks existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<Mark>,
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
        if !self.marks.is_empty() && self.kind == Kind::Video {
            return Err(Error::MarksOnVideo);
        }
        check_marks(&self.marks, source.width, source.height, limits)?;
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
    if video && !job.marks.is_empty() {
        return Err(Error::MarksOnVideo);
    }
    check_marks(&job.marks, source.width, source.height, limits)?;
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
        marks: job.marks.clone(),
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
        max_job_json: 1024 * 1024,
        max_marks: 32,
        max_stroke: 128,
        max_path_points: 256,
        max_points: 1024,
        max_text_pixels: 4_000_000,
        max_text_total: 8_000_000,
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

    fn square(x: u32, y: u32, width: u32, height: u32, stroke: u32) -> Mark {
        Mark::Square { rect: Rect { x, y, width, height }, color: [255, 0, 0], stroke }
    }
    fn blank(width: u32, height: u32) -> Vec<u8> {
        vec![0u8; (width * height * 4) as usize]
    }
    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * width + x) * 4) as usize;
        [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
    }

    #[test]
    fn marks_travel_in_the_plan_in_source_pixels() {
        let job = Job { marks: vec![square(2, 3, 10, 8, 2)], ..Default::default() };
        let made = plan(&job, &image(40, 30), &LIMITS).unwrap();
        assert_eq!(made.marks, job.marks);
        assert_eq!(made.check(&image(40, 30), &LIMITS), Ok(()));
        let none = plan(&Job::default(), &image(40, 30), &LIMITS).unwrap();
        assert!(none.marks.is_empty());
    }

    #[test]
    fn marks_outside_the_picture_or_badly_drawn_are_refused_not_clamped() {
        let try_mark = |mark: Mark| plan(&Job { marks: vec![mark], ..Default::default() }, &image(40, 30), &LIMITS);
        assert_eq!(try_mark(square(35, 0, 10, 5, 2)), Err(Error::MarkOutside));
        assert_eq!(try_mark(square(0, 28, 5, 5, 2)), Err(Error::MarkOutside));
        assert_eq!(try_mark(square(0, 0, 0, 5, 2)), Err(Error::MarkOutside));
        assert_eq!(try_mark(square(0, 0, 5, 0, 2)), Err(Error::MarkOutside));
        assert_eq!(try_mark(square(u32::MAX, 0, 5, 5, 2)), Err(Error::MarkOutside));
        assert_eq!(try_mark(square(0, 0, 5, 5, 0)), Err(Error::MarkStroke));
        assert_eq!(try_mark(square(0, 0, 5, 5, LIMITS.max_stroke + 1)), Err(Error::MarkStroke));
        assert!(try_mark(square(0, 0, 40, 30, LIMITS.max_stroke)).is_ok(), "the whole picture, thickest line");
    }

    #[test]
    fn there_is_a_cap_on_marks_and_none_on_video() {
        let many: Vec<Mark> = (0..=LIMITS.max_marks).map(|_| square(0, 0, 5, 5, 1)).collect();
        assert_eq!(
            plan(&Job { marks: many, ..Default::default() }, &image(40, 30), &LIMITS),
            Err(Error::TooManyMarks)
        );
        let at_cap: Vec<Mark> = (0..LIMITS.max_marks).map(|_| square(0, 0, 5, 5, 1)).collect();
        assert!(plan(&Job { marks: at_cap, ..Default::default() }, &image(40, 30), &LIMITS).is_ok());
        assert_eq!(
            plan(&Job { marks: vec![square(0, 0, 5, 5, 1)], ..Default::default() }, &video(64, 64, 5000), &LIMITS),
            Err(Error::MarksOnVideo)
        );
    }

    #[test]
    fn the_helper_checks_the_marks_it_is_sent() {
        let source = image(40, 30);
        let mut made = plan(&Job::default(), &source, &LIMITS).unwrap();
        made.marks = vec![square(30, 0, 20, 5, 2)];
        assert_eq!(made.check(&source, &LIMITS), Err(Error::MarkOutside));
        made.marks = vec![square(0, 0, 5, 5, 0)];
        assert_eq!(made.check(&source, &LIMITS), Err(Error::MarkStroke));
        made.marks = (0..=LIMITS.max_marks).map(|_| square(0, 0, 5, 5, 1)).collect();
        assert_eq!(made.check(&source, &LIMITS), Err(Error::TooManyMarks));
    }

    #[test]
    fn marks_are_part_of_the_json_and_unknown_shapes_or_fields_are_refused() {
        let job = Job { marks: vec![square(1, 2, 3, 4, 5)], ..Default::default() };
        let json = serde_json::to_vec(&job).unwrap();
        assert!(json.len() < LIMITS.max_job_json);
        assert_eq!(Job::from_json(&json, &LIMITS), Ok(job));
        assert_eq!(Job::from_json(b"{}", &LIMITS).map(|j| j.marks.len()), Ok(0), "older callers send none");
        for bad in [
            r#"{"marks":[{"shape":"star","rect":{"x":0,"y":0,"width":1,"height":1},"color":[0,0,0],"stroke":1}]}"#,
            r#"{"marks":[{"shape":"square","rect":{"x":0,"y":0,"width":1,"height":1},"color":[0,0,0],"stroke":1,"extra":1}]}"#,
            r#"{"marks":[{"shape":"square","rect":{"x":0,"y":0,"width":1,"height":1},"color":[0,0,300],"stroke":1}]}"#,
            r#"{"marks":[{"shape":"square","rect":{"x":0,"y":0,"width":1,"height":1},"color":[0,0],"stroke":1}]}"#,
        ] {
            assert_eq!(Job::from_json(bad.as_bytes(), &LIMITS), Err(Error::MalformedJob), "{bad}");
        }
    }

    #[test]
    fn a_square_is_drawn_as_an_outline_inside_its_rectangle() {
        let (w, h) = (20, 16);
        let mut rgba = blank(w, h);
        assert!(paint(&mut rgba, w, h, &[square(4, 3, 10, 8, 2)]));
        let red = [255, 0, 0, 255];
        let empty = [0, 0, 0, 0];
        // Corners and edges of the outline, two pixels thick, inside the rectangle.
        for (x, y) in [(4, 3), (13, 3), (4, 10), (13, 10), (5, 4), (8, 3), (8, 4), (4, 7), (5, 7), (13, 7), (12, 7), (8, 10), (8, 9)] {
            assert_eq!(pixel(&rgba, w, x, y), red, "({x},{y}) is on the outline");
        }
        // Inside the outline, outside the rectangle and the pixels just past each edge stay empty.
        for (x, y) in [(6, 5), (9, 6), (11, 8), (3, 3), (14, 3), (4, 2), (4, 11), (0, 0), (19, 15)] {
            assert_eq!(pixel(&rgba, w, x, y), empty, "({x},{y}) is not painted");
        }
        let painted = rgba.chunks(4).filter(|px| px[3] == 255).count();
        assert_eq!(painted, 10 * 8 - 6 * 4, "the 10x8 rectangle minus its 6x4 hollow");
    }

    #[test]
    fn a_thick_line_fills_a_small_square_and_never_leaves_it() {
        let (w, h) = (12, 12);
        let mut rgba = blank(w, h);
        assert!(paint(&mut rgba, w, h, &[square(2, 2, 6, 4, 100)]));
        let painted = rgba.chunks(4).filter(|px| px[3] == 255).count();
        assert_eq!(painted, 6 * 4, "the stroke cannot exceed the rectangle");
        assert_eq!(pixel(&rgba, w, 1, 2), [0, 0, 0, 0]);
        assert_eq!(pixel(&rgba, w, 8, 2), [0, 0, 0, 0]);
        assert_eq!(pixel(&rgba, w, 2, 6), [0, 0, 0, 0]);
    }

    #[test]
    fn later_marks_draw_over_earlier_ones_and_the_edges_clip() {
        let (w, h) = (10, 10);
        let mut rgba = blank(w, h);
        let blue = Mark::Square { rect: Rect { x: 0, y: 0, width: 10, height: 10 }, color: [0, 0, 255], stroke: 3 };
        assert!(paint(&mut rgba, w, h, &[blue, square(0, 0, 4, 4, 1)]));
        assert_eq!(pixel(&rgba, w, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&rgba, w, 9, 9), [0, 0, 255, 255]);
        // A rectangle that runs past the picture is clipped, not an error or a panic.
        let mut small = blank(4, 4);
        assert!(paint(&mut small, 4, 4, &[square(2, 2, 100, 100, 1)]));
        assert_eq!(pixel(&small, 4, 3, 3), [255, 0, 0, 255]);
    }

    #[test]
    fn painting_refuses_a_buffer_of_the_wrong_size_and_leaves_it_alone() {
        let mut rgba = blank(10, 10);
        assert!(!paint(&mut rgba, 10, 11, &[square(0, 0, 5, 5, 1)]));
        assert!(!paint(&mut rgba, 5, 5, &[square(0, 0, 5, 5, 1)]));
        assert!(!paint(&mut rgba[..399], 10, 10, &[square(0, 0, 5, 5, 1)]));
        assert!(rgba.iter().all(|byte| *byte == 0));
        assert!(!paint(&mut [], u32::MAX, u32::MAX, &[]), "a size that overflows is refused");
        assert!(paint(&mut rgba, 10, 10, &[]), "no marks is a successful no-op");
    }

    fn circle(x: u32, y: u32, width: u32, height: u32, stroke: u32) -> Mark {
        Mark::Circle { rect: Rect { x, y, width, height }, color: [0, 255, 0], stroke }
    }
    fn arrow(from: Point, to: Point, stroke: u32) -> Mark {
        Mark::Arrow { from, to, color: [0, 0, 255], stroke }
    }
    fn path(points: &[Point], stroke: u32) -> Mark {
        Mark::Freehand { points: points.to_vec(), color: [255, 255, 0], stroke, pressure: vec![] }
    }
    fn painted(rgba: &[u8]) -> usize {
        rgba.chunks(4).filter(|px| px[3] == 255).count()
    }

    #[test]
    fn a_circle_is_a_ring_inside_its_rectangle() {
        let (w, h) = (40, 40);
        let mut rgba = blank(w, h);
        assert!(paint(&mut rgba, w, h, &[circle(4, 4, 32, 32, 3)]));
        let green = [0, 255, 0, 255];
        // The four points where the ring touches the rectangle, and just inside them.
        for (x, y) in [(19, 4), (20, 4), (19, 35), (4, 19), (35, 20), (19, 6), (6, 19), (33, 20)] {
            assert_eq!(pixel(&rgba, w, x, y), green, "({x},{y}) is on the ring");
        }
        // The middle, the corners of the rectangle and the outside are empty.
        for (x, y) in [(20, 20), (12, 12), (4, 4), (35, 4), (4, 35), (35, 35), (0, 0), (39, 39), (19, 3), (19, 36)] {
            assert_eq!(pixel(&rgba, w, x, y), [0, 0, 0, 0], "({x},{y}) is empty");
        }
        let ring = painted(&rgba);
        assert!(ring > 200 && ring < 32 * 32, "a ring, not a disc or a square: {ring} pixels");
        // Symmetric left to right and top to bottom.
        for y in 0..h {
            for x in 0..w {
                assert_eq!(pixel(&rgba, w, x, y)[3], pixel(&rgba, w, w - 1 - x, y)[3], "mirror at ({x},{y})");
                assert_eq!(pixel(&rgba, w, x, y)[3], pixel(&rgba, w, x, h - 1 - y)[3], "flip at ({x},{y})");
            }
        }
    }

    #[test]
    fn an_ellipse_follows_a_wide_rectangle_and_a_thick_line_fills_it() {
        let (w, h) = (40, 20);
        let mut rgba = blank(w, h);
        assert!(paint(&mut rgba, w, h, &[circle(0, 0, 40, 20, 2)]));
        assert_eq!(pixel(&rgba, w, 19, 0)[3], 255, "top of the ellipse");
        assert_eq!(pixel(&rgba, w, 0, 9)[3], 255, "left tip");
        assert_eq!(pixel(&rgba, w, 20, 10)[3], 0, "hollow");
        assert_eq!(pixel(&rgba, w, 0, 0)[3], 0, "the corner is outside an ellipse");
        let mut thick = blank(10, 10);
        assert!(paint(&mut thick, 10, 10, &[circle(0, 0, 10, 10, 100)]));
        assert_eq!(pixel(&thick, 10, 5, 5)[3], 255, "a stroke wider than the circle fills it");
        assert_eq!(pixel(&thick, 10, 0, 0)[3], 0, "but only the ellipse, not the corners");
    }

    #[test]
    fn an_arrow_has_a_shaft_and_a_head_that_points_at_its_end() {
        let (w, h) = (60, 30);
        let mut rgba = blank(w, h);
        assert!(paint(&mut rgba, w, h, &[arrow((5, 15), (50, 15), 2)]));
        let blue = [0, 0, 255, 255];
        for x in [5, 6, 20, 35, 49, 50] {
            assert_eq!(pixel(&rgba, w, x, 15), blue, "({x},15) is on the shaft");
        }
        // The head's two sides run back from the tip, above and below the shaft.
        assert_eq!(pixel(&rgba, w, 44, 12)[3], 255, "upper side of the head");
        assert_eq!(pixel(&rgba, w, 44, 18)[3], 255, "lower side of the head");
        assert_eq!(pixel(&rgba, w, 30, 8)[3], 0, "away from the shaft");
        assert_eq!(pixel(&rgba, w, 30, 22)[3], 0);
        assert_eq!(pixel(&rgba, w, 2, 15)[3], 0, "nothing behind the tail");
        assert_eq!(pixel(&rgba, w, 56, 15)[3], 0, "nothing past the tip");
        // A head is wider than the shaft and only at the tip end.
        let column = |x: u32| (0..h).filter(|y| pixel(&rgba, w, x, *y)[3] == 255).count();
        assert!(column(44) > column(20), "the head is wider than the shaft");
    }

    #[test]
    fn an_arrow_points_the_right_way_in_every_direction() {
        let (w, h) = (81, 81);
        // (tip, unit direction the arrow travels): right, left, up, down.
        for (tip, dir) in [((70i64, 40i64), (1i64, 0i64)), ((10, 40), (-1, 0)), ((40, 10), (0, -1)), ((40, 70), (0, 1))] {
            let tail = (tip.0 - dir.0 * 50, tip.1 - dir.1 * 50);
            let mut rgba = blank(w, h);
            assert!(paint(&mut rgba, w, h, &[arrow((tail.0 as u32, tail.1 as u32), (tip.0 as u32, tip.1 as u32), 2)]));
            let on = |x: i64, y: i64| pixel(&rgba, w, x as u32, y as u32)[3] == 255;
            assert!(on(tip.0, tip.1) && on(tail.0, tail.1), "both ends of the shaft ({tip:?})");
            // Eight pixels back from the tip the head's sides are about four pixels to each side.
            let (back_x, back_y) = (tip.0 - dir.0 * 8, tip.1 - dir.1 * 8);
            let side = (-dir.1, dir.0);
            assert!(on(back_x + side.0 * 4, back_y + side.1 * 4), "one side of the head ({tip:?})");
            assert!(on(back_x - side.0 * 4, back_y - side.1 * 4), "the other side ({tip:?})");
            // Past the tip there is nothing, and four pixels either side of the shaft is empty at the tail.
            assert!(!on(tail.0 + side.0 * 4, tail.1 + side.1 * 4), "the tail end is a plain line ({tip:?})");
        }
    }

    #[test]
    fn a_freehand_line_joins_its_points_with_round_ends() {
        let (w, h) = (30, 30);
        let mut rgba = blank(w, h);
        assert!(paint(&mut rgba, w, h, &[path(&[(5, 5), (25, 5), (25, 25)], 4)]));
        let yellow = [255, 255, 0, 255];
        for (x, y) in [(5, 5), (15, 5), (25, 5), (25, 15), (25, 24), (24, 6), (15, 4), (15, 6)] {
            assert_eq!(pixel(&rgba, w, x, y), yellow, "({x},{y}) is on the line");
        }
        for (x, y) in [(5, 25), (10, 15), (15, 15), (0, 0), (29, 29), (15, 9), (20, 20)] {
            assert_eq!(pixel(&rgba, w, x, y)[3], 0, "({x},{y}) is off the line");
        }
        // A single point is a dot of the stroke's size.
        let mut dot = blank(w, h);
        assert!(paint(&mut dot, w, h, &[path(&[(15, 15)], 6)]));
        assert_eq!(pixel(&dot, w, 15, 15)[3], 255);
        assert!(painted(&dot) >= 20 && painted(&dot) <= 40, "about a 6 px disc: {}", painted(&dot));
    }

    #[test]
    fn strokes_that_run_past_the_picture_are_clipped_without_a_panic() {
        let mut rgba = blank(10, 10);
        assert!(paint(&mut rgba, 10, 10, &[arrow((0, 0), (10, 10), 128), path(&[(0, 10), (10, 0)], 128)]));
        assert!(painted(&rgba) <= 100);
        let mut edge = blank(10, 10);
        assert!(paint(&mut edge, 10, 10, &[path(&[(10, 10), (10, 0)], 2)]));
        assert_eq!(pixel(&edge, 10, 9, 5)[3], 255, "a stroke on the far edge reaches the last pixel column");
    }

    #[test]
    fn the_new_shapes_are_validated_against_the_picture() {
        let check = |mark: Mark| plan(&Job { marks: vec![mark], ..Default::default() }, &image(40, 30), &LIMITS);
        assert!(check(circle(0, 0, 40, 30, 2)).is_ok());
        assert_eq!(check(circle(30, 0, 20, 10, 2)), Err(Error::MarkOutside));
        assert_eq!(check(circle(0, 0, 10, 10, 0)), Err(Error::MarkStroke));
        assert!(check(arrow((0, 0), (40, 30), 2)).is_ok(), "the far corner is allowed");
        assert_eq!(check(arrow((0, 0), (41, 30), 2)), Err(Error::MarkOutside));
        assert_eq!(check(arrow((5, 5), (5, 5), 2)), Err(Error::MarkOutside), "an arrow needs a length");
        assert_eq!(check(arrow((0, 0), (10, 10), 129)), Err(Error::MarkStroke));
        assert!(check(path(&[(1, 1)], 2)).is_ok());
        assert_eq!(check(path(&[], 2)), Err(Error::MarkOutside));
        assert_eq!(check(path(&[(1, 1), (40, 31)], 2)), Err(Error::MarkOutside));
        assert_eq!(check(path(&[(1, 1), (u32::MAX, 0)], 2)), Err(Error::MarkOutside));
        assert_eq!(check(path(&[(1, 1), (2, 2)], 0)), Err(Error::MarkStroke));
    }

    #[test]
    fn path_points_are_capped_per_stroke_and_overall() {
        let line = |count: usize| path(&(0..count).map(|i| ((i % 40) as u32, (i % 30) as u32)).collect::<Vec<_>>(), 2);
        let check = |marks: Vec<Mark>| plan(&Job { marks, ..Default::default() }, &image(40, 30), &LIMITS);
        assert!(check(vec![line(LIMITS.max_path_points)]).is_ok());
        assert_eq!(check(vec![line(LIMITS.max_path_points + 1)]), Err(Error::MarkOutside));
        // Four full strokes reach the overall cap; one more point past it is refused.
        let full = || line(LIMITS.max_path_points);
        assert_eq!(LIMITS.max_points, 4 * LIMITS.max_path_points);
        assert!(check(vec![full(), full(), full(), full()]).is_ok());
        assert_eq!(check(vec![full(), full(), full(), full(), path(&[(1, 1)], 2)]), Err(Error::TooManyMarks));
        // An arrow counts as two points.
        assert_eq!(check(vec![full(), full(), full(), line(LIMITS.max_path_points - 1), arrow((0, 0), (9, 9), 2)]), Err(Error::TooManyMarks));
        assert!(check(vec![full(), full(), full(), line(LIMITS.max_path_points - 2), arrow((0, 0), (9, 9), 2)]).is_ok());
    }

    #[test]
    fn a_full_set_of_marks_fits_in_the_job_description() {
        let strokes: Vec<Mark> = (0..4).map(|_| path(&(0..256).map(|i| (3000 + i, 3000 + i)).collect::<Vec<_>>(), 100)).collect();
        let job = Job { marks: strokes, ..Default::default() };
        let json = serde_json::to_vec(&job).unwrap();
        assert!(json.len() < LIMITS.max_job_json, "{} bytes", json.len());
        assert_eq!(Job::from_json(&json, &LIMITS), Ok(job));
    }

    #[test]
    fn every_shape_round_trips_through_json_and_unknown_fields_are_refused() {
        let job = Job {
            marks: vec![square(1, 2, 3, 4, 5), circle(1, 2, 3, 4, 5), arrow((1, 2), (3, 4), 5), path(&[(1, 2), (3, 4)], 5)],
            ..Default::default()
        };
        let json = serde_json::to_string(&job).unwrap();
        assert!(json.contains(r#""shape":"circle""#) && json.contains(r#""shape":"arrow""#) && json.contains(r#""shape":"freehand""#));
        assert_eq!(Job::from_json(json.as_bytes(), &LIMITS), Ok(job));
        for bad in [
            r#"{"marks":[{"shape":"arrow","from":[1,2],"to":[3,4],"color":[0,0,0],"stroke":1,"extra":1}]}"#,
            r#"{"marks":[{"shape":"arrow","from":[1,2,3],"to":[3,4],"color":[0,0,0],"stroke":1}]}"#,
            r#"{"marks":[{"shape":"arrow","from":[-1,2],"to":[3,4],"color":[0,0,0],"stroke":1}]}"#,
            r#"{"marks":[{"shape":"freehand","points":[[1,2],[3]],"color":[0,0,0],"stroke":1}]}"#,
            r#"{"marks":[{"shape":"freehand","points":"nope","color":[0,0,0],"stroke":1}]}"#,
            r#"{"marks":[{"shape":"circle","rect":{"x":0,"y":0,"width":1,"height":1},"color":[0,0,0]}]}"#,
        ] {
            assert_eq!(Job::from_json(bad.as_bytes(), &LIMITS), Err(Error::MalformedJob), "{bad}");
        }
    }

    fn pen(points: &[Point], pressure: &[u8], stroke: u32) -> Mark {
        Mark::Freehand { points: points.to_vec(), color: [255, 255, 0], stroke, pressure: pressure.to_vec() }
    }
    fn height_of_line_at(rgba: &[u8], w: u32, h: u32, x: u32) -> usize {
        (0..h).filter(|y| pixel(rgba, w, x, *y)[3] == 255).count()
    }

    #[test]
    fn a_pen_line_is_as_wide_as_the_pressure_says() {
        let (w, h) = (80, 40);
        let mut rgba = blank(w, h);
        // Light at the left, hard at the right, a 20 px stroke.
        assert!(paint(&mut rgba, w, h, &[pen(&[(5, 20), (75, 20)], &[26, 255], 20)]));
        let light = height_of_line_at(&rgba, w, h, 8);
        let middle = height_of_line_at(&rgba, w, h, 40);
        let hard = height_of_line_at(&rgba, w, h, 72);
        assert!(light >= 1 && light <= 4, "a light touch is thin: {light}");
        assert!(hard >= 17 && hard <= 21, "a hard press is the full 20: {hard}");
        assert!(light < middle && middle < hard, "it widens smoothly: {light} {middle} {hard}");
    }

    #[test]
    fn without_pressure_a_line_keeps_one_width_and_equal_pressure_matches_it() {
        let (w, h) = (60, 30);
        let (mut plain, mut full) = (blank(w, h), blank(w, h));
        assert!(paint(&mut plain, w, h, &[path(&[(5, 15), (55, 15)], 8)]));
        assert!(paint(&mut full, w, h, &[pen(&[(5, 15), (55, 15)], &[255, 255], 8)]));
        assert_eq!(plain, full, "full pressure everywhere is the same as no pressure data");
        assert_eq!(height_of_line_at(&plain, w, h, 10), height_of_line_at(&plain, w, h, 50));
    }

    #[test]
    fn a_pen_line_never_vanishes_and_a_single_touch_makes_a_dot() {
        let (w, h) = (40, 20);
        let mut rgba = blank(w, h);
        assert!(paint(&mut rgba, w, h, &[pen(&[(5, 10), (35, 10)], &[1, 1], 20)]));
        assert!(painted(&rgba) >= 30, "the faintest pressure still leaves a line: {}", painted(&rgba));
        let mut dot = blank(w, h);
        assert!(paint(&mut dot, w, h, &[pen(&[(20, 10)], &[255], 10)]));
        assert_eq!(pixel(&dot, w, 20, 10)[3], 255);
        assert!(painted(&dot) >= 60, "a 10 px dot: {}", painted(&dot));
    }

    #[test]
    fn pressure_values_must_match_the_points_and_be_usable() {
        let check = |mark: Mark| plan(&Job { marks: vec![mark], ..Default::default() }, &image(40, 30), &LIMITS);
        assert!(check(pen(&[(1, 1), (9, 9)], &[10, 200], 4)).is_ok());
        assert!(check(pen(&[(1, 1), (9, 9)], &[], 4)).is_ok(), "no pressure is a mouse line");
        assert_eq!(check(pen(&[(1, 1), (9, 9)], &[10], 4)), Err(Error::MarkPressure), "too few");
        assert_eq!(check(pen(&[(1, 1), (9, 9)], &[10, 20, 30], 4)), Err(Error::MarkPressure), "too many");
        assert_eq!(check(pen(&[(1, 1), (9, 9)], &[10, 0], 4)), Err(Error::MarkPressure), "zero is not a pressure");
    }

    #[test]
    fn pressure_round_trips_and_is_left_out_when_there_is_none() {
        let with = Job { marks: vec![pen(&[(1, 2), (3, 4)], &[10, 250], 5)], ..Default::default() };
        let json = serde_json::to_string(&with).unwrap();
        assert!(json.contains(r#""pressure":[10,250]"#));
        assert_eq!(Job::from_json(json.as_bytes(), &LIMITS), Ok(with));
        let without = Job { marks: vec![path(&[(1, 2), (3, 4)], 5)], ..Default::default() };
        assert!(!serde_json::to_string(&without).unwrap().contains("pressure"), "older helpers never see the field");
        let old = r#"{"marks":[{"shape":"freehand","points":[[1,2],[3,4]],"color":[0,0,0],"stroke":1}]}"#;
        assert!(Job::from_json(old.as_bytes(), &LIMITS).is_ok(), "a request without pressure still reads");
        assert_eq!(Job::from_json(br#"{"marks":[{"shape":"freehand","points":[[1,2]],"color":[0,0,0],"stroke":1,"pressure":[300]}]}"#, &LIMITS), Err(Error::MalformedJob), "a level is a byte");
    }

    #[test]
    fn a_plan_without_marks_carries_no_marks_field_for_an_older_helper() {
        let made = plan(&Job::default(), &image(40, 30), &LIMITS).unwrap();
        let json = serde_json::to_string(&made).unwrap();
        assert!(!json.contains("marks"), "{json}");
        let marked = plan(&Job { marks: vec![square(0, 0, 5, 5, 1)], ..Default::default() }, &image(40, 30), &LIMITS).unwrap();
        assert!(serde_json::to_string(&marked).unwrap().contains("marks"));
    }

    fn text_mark(x: u32, y: u32, width: u32, height: u32, coverage: &[u8]) -> Mark {
        Mark::Text { rect: Rect { x, y, width, height }, color: [255, 255, 255], mask: encode_mask(coverage) }
    }

    #[test]
    fn a_mask_survives_packing_and_only_the_exact_size_unpacks() {
        let coverage: Vec<u8> = (0..120).map(|i| (i * 2) as u8).collect();
        let packed = encode_mask(&coverage);
        assert_eq!(decode_mask(&packed, 12, 10), Some(coverage.clone()));
        assert_eq!(decode_mask(&packed, 10, 12), Some(coverage.clone()), "only the pixel count matters here");
        assert_eq!(decode_mask(&packed, 12, 9), None, "too many bytes for the size");
        assert_eq!(decode_mask(&packed, 12, 11), None, "too few");
        assert_eq!(decode_mask(&packed, 0, 10), None);
        assert_eq!(decode_mask("not base64 !!", 12, 10), None);
        assert_eq!(decode_mask("AAAA", 12, 10), None, "base64 of something that is not deflate");
        assert_eq!(decode_mask("", 12, 10), None);
        assert_eq!(decode_mask(&packed, u32::MAX, u32::MAX), None, "a size that overflows is refused");
    }

    #[test]
    fn a_mask_that_inflates_far_past_its_size_is_stopped() {
        // Small when packed, so the length check passes, but 200 000 bytes when inflated.
        let bomb = encode_mask(&vec![0u8; 200_000]);
        assert!(bomb.len() < 2_000, "it packs tiny: {} bytes", bomb.len());
        assert_eq!(decode_mask(&bomb, 100, 100), None, "not 100x100, and never inflated past one byte over it");
        // A truly enormous one is refused before it is even decoded.
        let huge = encode_mask(&vec![0u8; 50_000_000]);
        assert_eq!(decode_mask(&huge, 100, 100), None);
        assert_eq!(decode_mask(&bomb, 200, 1000), Some(vec![0u8; 200_000]), "the same bytes are fine at their true size");
    }

    #[test]
    fn text_is_composited_over_the_picture_in_its_colour_by_coverage() {
        let (w, h) = (10, 6);
        let mut rgba: Vec<u8> = (0..w * h).flat_map(|_| [0u8, 0, 200, 255]).collect();
        // A 4x2 patch at (3, 2): full, half, a quarter, none; then the same in the second row.
        let coverage = [255, 128, 64, 0, 255, 128, 64, 0];
        assert!(paint(&mut rgba, w, h, &[text_mark(3, 2, 4, 2, &coverage)]));
        assert_eq!(pixel(&rgba, w, 3, 2), [255, 255, 255, 255], "full coverage is the text colour");
        let half = pixel(&rgba, w, 4, 2);
        assert!(half[0] > 120 && half[0] < 135 && half[2] > 225, "half way between blue and white: {half:?}");
        let quarter = pixel(&rgba, w, 5, 2);
        assert!(quarter[0] > 55 && quarter[0] < 72, "{quarter:?}");
        assert_eq!(pixel(&rgba, w, 6, 2), [0, 0, 200, 255], "no coverage leaves the picture alone");
        assert_eq!(pixel(&rgba, w, 3, 3)[0], 255, "the second row too");
        for (x, y) in [(2, 2), (7, 2), (3, 1), (3, 4), (0, 0)] {
            assert_eq!(pixel(&rgba, w, x, y), [0, 0, 200, 255], "({x},{y}) is outside the patch");
        }
    }

    #[test]
    fn text_over_a_transparent_picture_keeps_the_transparency_it_does_not_cover() {
        let (w, h) = (4, 1);
        let mut rgba = blank(w, h);
        assert!(paint(&mut rgba, w, h, &[text_mark(0, 0, 4, 1, &[255, 128, 0, 255])]));
        assert_eq!(pixel(&rgba, w, 0, 0), [255, 255, 255, 255]);
        let half = pixel(&rgba, w, 1, 0);
        assert!(half[3] > 120 && half[3] < 135, "half covered is half opaque: {half:?}");
        assert_eq!(half[0], 255, "and still the text colour, not darkened by the empty pixel");
        assert_eq!(pixel(&rgba, w, 2, 0), [0, 0, 0, 0], "uncovered stays transparent");
    }

    #[test]
    fn a_text_mark_is_checked_for_position_size_and_a_mask_that_fits() {
        let check = |mark: Mark| plan(&Job { marks: vec![mark], ..Default::default() }, &image(40, 30), &LIMITS);
        assert!(check(text_mark(0, 0, 40, 30, &vec![9; 1200])).is_ok());
        assert_eq!(check(text_mark(35, 0, 10, 2, &[0; 20])), Err(Error::MarkOutside));
        assert_eq!(check(text_mark(0, 0, 0, 2, &[])), Err(Error::MarkOutside));
        assert_eq!(check(text_mark(0, 0, 4, 2, &[0; 7])), Err(Error::MarkText), "a mask one byte short");
        assert_eq!(check(text_mark(0, 0, 4, 2, &[0; 9])), Err(Error::MarkText), "and one byte long");
        let broken = Mark::Text { rect: Rect { x: 0, y: 0, width: 4, height: 2 }, color: [0, 0, 0], mask: "nonsense".into() };
        assert_eq!(check(broken), Err(Error::MarkText));
    }

    #[test]
    fn text_bitmaps_are_capped_each_and_together() {
        let small = Limits { max_text_pixels: 100, max_text_total: 150, ..LIMITS };
        let check = |marks: Vec<Mark>| plan(&Job { marks, ..Default::default() }, &image(400, 300), &small);
        assert!(check(vec![text_mark(0, 0, 10, 10, &[1; 100])]).is_ok(), "exactly the cap");
        assert_eq!(check(vec![text_mark(0, 0, 11, 10, &[1; 110])]), Err(Error::MarkText), "one over");
        assert_eq!(check(vec![text_mark(0, 0, 10, 8, &[1; 80]), text_mark(20, 0, 10, 8, &[1; 80])]), Err(Error::MarkText), "160 pixels together");
        assert!(check(vec![text_mark(0, 0, 10, 7, &[1; 70]), text_mark(20, 0, 10, 8, &[1; 80])]).is_ok(), "150 together is allowed");
    }

    #[test]
    fn a_large_but_plain_text_job_still_fits_in_the_job_description() {
        // A 2000x400 headline of mostly empty pixels packs small.
        let mut coverage = vec![0u8; 2000 * 400];
        for row in 100..300 {
            for column in (50..1950).step_by(7) {
                coverage[row * 2000 + column] = 255;
            }
        }
        let job = Job { marks: vec![text_mark(10, 10, 2000, 400, &coverage)], ..Default::default() };
        let json = serde_json::to_vec(&job).unwrap();
        assert!(json.len() < LIMITS.max_job_json, "{} bytes", json.len());
        assert!(json.len() < 100_000, "it should pack well, got {}", json.len());
        assert!(plan(&job, &image(2100, 500), &LIMITS).is_ok());
    }

    #[test]
    fn text_marks_round_trip_and_unknown_fields_are_refused() {
        let job = Job { marks: vec![text_mark(1, 2, 3, 2, &[0, 128, 255, 9, 9, 9])], ..Default::default() };
        let json = serde_json::to_string(&job).unwrap();
        assert!(json.contains(r#""shape":"text""#));
        assert_eq!(Job::from_json(json.as_bytes(), &LIMITS), Ok(job));
        let bad = r#"{"marks":[{"shape":"text","rect":{"x":0,"y":0,"width":1,"height":1},"color":[0,0,0],"mask":"AA==","extra":1}]}"#;
        assert_eq!(Job::from_json(bad.as_bytes(), &LIMITS), Err(Error::MalformedJob));
    }

    #[test]
    fn painting_a_damaged_text_mask_fails_instead_of_drawing_garbage() {
        let mut rgba = blank(8, 8);
        let broken = Mark::Text { rect: Rect { x: 0, y: 0, width: 4, height: 2 }, color: [255, 0, 0], mask: "AAAA".into() };
        assert!(!paint(&mut rgba, 8, 8, &[broken]));
    }
}
