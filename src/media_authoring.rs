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
        })
    }
}
impl std::error::Error for Error {}

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
