//! Runs picture authoring in the confined `monogram-media-author` helper.
//!
//! The helper never runs in this process. It is started inside bubblewrap with no network, the
//! source mounted read-only, one fresh private output directory and only the few GStreamer
//! plugin files the picture chain needs, so no other plugin or codec can be loaded. Everything the
//! helper reports is re-validated here, and the produced file is checked again from its own
//! header. Callers must run this on a worker thread: it blocks until the helper exits.
use crate::media_authoring::{self, Format, Job, Kind, Limits, Plan, SourceInfo};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

const HELPER: &str = "monogram-media-author";
/// Slightly above the helper's own alarm, so a hung helper is killed from outside too.
const DEADLINE: Duration = Duration::from_secs(40);

/// Plugin files the picture chain needs, by the names used across distributions. Missing ones are
/// skipped: the helper then fails to build its chain and editing is reported unavailable.
const PLUGINS: &[&str] = &[
    "libgstcoreelements.so",
    "libgstpng.so",
    "libgstjpeg.so",
    "libgstjpegformat.so",
    "libgstwebp.so",
    "libgstvideoconvertscale.so",
    "libgstvideoconvert.so",
    "libgstvideoscale.so",
    "libgstvideocrop.so",
    "libgstvideofilter.so",
];

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The helper or bubblewrap is not installed, or the source cannot be used.
    Unavailable,
    /// The request or the file was refused by the limits.
    Refused(media_authoring::Error),
    /// The helper rejected or could not process the file.
    Failed,
    /// The helper refused a request this build wrote: it is an older build that does not know a
    /// field. `cargo run` rebuilds only the app, so the helper next to it can lag behind.
    OutOfDate,
    TimedOut,
    Cancelled,
    /// The helper claimed success but its output did not match the plan.
    BadOutput,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => f.write_str("Picture editing is unavailable"),
            Self::Refused(error) => error.fmt(f),
            Self::Failed => f.write_str("This picture could not be edited"),
            Self::OutOfDate => f.write_str("The picture editing helper is out of date. Rebuild with cargo build, then restart"),
            Self::TimedOut => f.write_str("Editing this picture took too long"),
            Self::Cancelled => f.write_str("Editing was cancelled"),
            Self::BadOutput => f.write_str("The edited picture could not be verified"),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Debug)]
pub struct Authored {
    pub path: PathBuf,
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
}

fn helper() -> Option<PathBuf> {
    crate::confined::helper(HELPER, "MONOGRAM_MEDIA_AUTHOR_HELPER")
}
fn bwrap() -> Option<PathBuf> {
    crate::confined::bwrap()
}
fn plugin_dirs() -> Vec<PathBuf> {
    crate::confined::plugin_dirs()
}
fn plugin_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    crate::confined::plugin_files(PLUGINS, dirs)
}
/// The bubblewrap command line (see `confined`); separate from spawning so tests can inspect it.
fn sandbox_command(
    bwrap: &Path,
    helper: &Path,
    source: &Path,
    out_dir: &Path,
    plugins: &[PathBuf],
    mode: &str,
) -> Command {
    crate::confined::sandbox_command(bwrap, helper, crate::confined::Input::Path(source), Some(out_dir), plugins, &[mode])
}

/// Wait for the helper, killing it on cancellation or after the deadline.
fn wait(child: &mut Child, cancel: &dyn Fn() -> bool) -> Result<Option<i32>, Error> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|_| Error::Failed)? {
            return Ok(status.code());
        }
        let stop = if cancel() {
            Some(Error::Cancelled)
        } else if started.elapsed() > DEADLINE {
            Some(Error::TimedOut)
        } else {
            None
        };
        if let Some(error) = stop {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn execute(
    source: &Path,
    out_dir: &Path,
    mode: &str,
    input: &[u8],
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>, Error> {
    let (Some(helper), Some(bwrap)) = (helper(), bwrap()) else {
        return Err(Error::Unavailable);
    };
    let source = source.canonicalize().map_err(|_| Error::Unavailable)?;
    if !source.is_file() {
        return Err(Error::Unavailable);
    }
    let plugins = plugin_files(&plugin_dirs());
    let mut child = sandbox_command(&bwrap, &helper, &source, out_dir, &plugins, mode)
        .spawn()
        .map_err(|_| Error::Unavailable)?;
    // The request is a few hundred bytes, far below the pipe buffer, so this cannot block.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    let mut stdout = child.stdout.take().ok_or(Error::Failed)?;
    let reader = std::thread::spawn(move || {
        let mut reply = Vec::new();
        // The helper prints one short JSON line; anything beyond this is ignored.
        let _ = (&mut stdout).take(4096).read_to_end(&mut reply);
        reply
    });
    match wait(&mut child, cancel)? {
        Some(0) => {}
        code => {
            let _ = reader.join();
            // Exit code 3 is "bad request": this build checked the plan itself, so the helper
            // must be older than the app.
            return Err(if code == Some(3) { Error::OutOfDate } else { Error::Failed });
        }
    }
    reader.join().map_err(|_| Error::Failed)
}

/// Describe a picture without decoding it outside the sandbox.
pub fn probe(source: &Path, cancel: &dyn Fn() -> bool) -> Result<SourceInfo, Error> {
    let scratch = tempdir()?;
    let reply = execute(source, &scratch.0, "probe", b"", cancel)?;
    let info: SourceInfo = serde_json::from_slice(&reply).map_err(|_| Error::Failed)?;
    if info.kind != Kind::Image {
        return Err(Error::Failed);
    }
    Ok(info)
}

/// Apply `job` to the picture at `source`, writing the result inside `work_dir`. The caller owns
/// the returned file and should delete it when finished.
pub fn author(
    source: &Path,
    job: &Job,
    work_dir: &Path,
    cancel: &dyn Fn() -> bool,
) -> Result<Authored, Error> {
    let limits = Limits::default();
    let info = probe(source, cancel)?;
    let plan = media_authoring::plan(job, &info, &limits).map_err(Error::Refused)?;
    let scratch = tempdir_in(work_dir)?;
    let request = serde_json::to_vec(&serde_json::json!({ "plan": plan, "source": info }))
        .map_err(|_| Error::Failed)?;
    let reply = execute(source, &scratch.0, "run", &request, cancel)?;
    let name = serde_json::from_slice::<serde_json::Value>(&reply)
        .ok()
        .and_then(|value| value["file"].as_str().map(str::to_owned))
        .ok_or(Error::Failed)?;
    if !matches!(name.as_str(), "result.png" | "result.jpg") {
        return Err(Error::BadOutput);
    }
    let produced = scratch.0.join(&name);
    let authored = verify(&produced, &plan, &limits)?;
    // Move the verified file out of the scratch directory, which is then removed.
    let kept = work_dir.join(format!("authored-{}.{}", uuid::Uuid::new_v4(), &name["result.".len()..]));
    std::fs::rename(&produced, &kept).map_err(|_| Error::BadOutput)?;
    Ok(Authored { path: kept, ..authored })
}

/// Trust the helper's output only after reading the file's own header: a regular file, within the
/// size cap, of a picture format, with exactly the planned dimensions.
fn verify(path: &Path, plan: &Plan, limits: &Limits) -> Result<Authored, Error> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| Error::BadOutput)?;
    if !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > plan.max_output_bytes.min(limits.max_output_bytes)
    {
        return Err(Error::BadOutput);
    }
    let mut head = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(media_authoring::SNIFF_BYTES as u64).read_to_end(&mut head))
        .map_err(|_| Error::BadOutput)?;
    let (format, width, height) = media_authoring::sniff_image(&head).map_err(|_| Error::BadOutput)?;
    if (width, height) != (plan.output_width, plan.output_height) || format == Format::Webp {
        return Err(Error::BadOutput);
    }
    Ok(Authored { path: path.into(), format, width, height, bytes: metadata.len() })
}

/// A private scratch directory removed on drop, including after a failure.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn tempdir() -> Result<Scratch, Error> {
    tempdir_in(&std::env::temp_dir())
}
fn tempdir_in(parent: &Path) -> Result<Scratch, Error> {
    use std::os::unix::fs::DirBuilderExt;
    let path = parent.join(format!(".monogram-author-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .map_err(|_| Error::Unavailable)?;
    Ok(Scratch(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_authoring::{Mark, Rect, Rotation};

    fn args(command: &Command) -> Vec<String> {
        command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn the_sandbox_has_no_network_one_writable_directory_and_only_listed_plugins() {
        let command = sandbox_command(
            Path::new("/usr/bin/bwrap"),
            Path::new("/opt/helper"),
            Path::new("/home/me/photo.png"),
            Path::new("/tmp/scratch"),
            &[PathBuf::from("/usr/lib/gstreamer-1.0/libgstpng.so")],
            "run",
        );
        let args = args(&command);
        let has = |needle: &[&str]| args.windows(needle.len()).any(|w| w == needle);
        for flag in ["--unshare-all", "--die-with-parent", "--new-session", "--clearenv"] {
            assert!(args.iter().any(|arg| arg == flag), "{flag}");
        }
        assert!(has(&["--cap-drop", "ALL"]));
        assert!(!args.iter().any(|arg| arg == "--share-net"), "network stays unshared");
        assert!(has(&["--ro-bind", "/home/me/photo.png", "/in/source"]), "source is read-only");
        assert!(has(&["--bind", "/tmp/scratch", "/out"]), "one writable directory");
        assert_eq!(args.iter().filter(|arg| *arg == "--bind").count(), 1);
        assert!(has(&["--ro-bind", "/usr/lib/gstreamer-1.0/libgstpng.so", "/plugins/libgstpng.so"]));
        assert!(has(&["--setenv", "GST_PLUGIN_SYSTEM_PATH_1_0", "/plugins"]));
        assert!(has(&["--setenv", "GST_REGISTRY_FORK", "no"]), "no plugin scanner process");
        assert_eq!(args.last().map(String::as_str), Some("run"));
    }

    #[test]
    fn plugin_files_are_found_by_name_across_search_directories() {
        let root = std::env::temp_dir().join(format!("plugin-test-{}", uuid::Uuid::new_v4()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("libgstpng.so"), b"x").unwrap();
        std::fs::write(b.join("libgstjpeg.so"), b"x").unwrap();
        std::fs::write(b.join("libgstevil.so"), b"x").unwrap();
        let found = plugin_files(&[a.clone(), b.clone()]);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(found, [a.join("libgstpng.so"), b.join("libgstjpeg.so")]);
    }

    #[test]
    fn output_is_trusted_only_when_its_own_header_matches_the_plan() {
        let dir = tempdir().unwrap();
        let plan = Plan {
            kind: Kind::Image,
            crop: Rect { x: 0, y: 0, width: 10, height: 10 },
            rotation: Rotation::None,
            trim: None,
            output_width: 10,
            output_height: 10,
            max_output_bytes: 1024,
            marks: vec![],
        };
        let png = |width: u32, height: u32| {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            bytes.extend_from_slice(&13u32.to_be_bytes());
            bytes.extend_from_slice(b"IHDR");
            bytes.extend_from_slice(&width.to_be_bytes());
            bytes.extend_from_slice(&height.to_be_bytes());
            bytes
        };
        let file = dir.0.join("result.png");
        std::fs::write(&file, png(10, 10)).unwrap();
        assert_eq!(verify(&file, &plan, &Limits::default()).unwrap().width, 10);
        std::fs::write(&file, png(11, 10)).unwrap();
        assert_eq!(verify(&file, &plan, &Limits::default()).unwrap_err(), Error::BadOutput, "wrong size");
        std::fs::write(&file, b"not a picture").unwrap();
        assert_eq!(verify(&file, &plan, &Limits::default()).unwrap_err(), Error::BadOutput);
        std::fs::write(&file, vec![0u8; 2048]).unwrap();
        assert_eq!(verify(&file, &plan, &Limits::default()).unwrap_err(), Error::BadOutput, "over the cap");
        let link = dir.0.join("link.png");
        std::fs::write(&file, png(10, 10)).unwrap();
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(verify(&link, &plan, &Limits::default()).unwrap_err(), Error::BadOutput, "symlinks are refused");
    }

    #[test]
    fn scratch_directories_are_private_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;
        let path = {
            let scratch = tempdir().unwrap();
            assert_eq!(std::fs::metadata(&scratch.0).unwrap().permissions().mode() & 0o777, 0o700);
            scratch.0.clone()
        };
        assert!(!path.exists());
    }

    // The tests below run the real helper in bubblewrap; build it first with `cargo build --bins`.
    const NEVER: &dyn Fn() -> bool = &|| false;
    fn fixture(name: &str, bytes: &[u8]) -> (Scratch, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }
    fn gradient_png() -> Vec<u8> {
        // 64x32 RGB gradient, written by the scratchpad generator and embedded as hex.
        include_bytes!("../tests/fixtures/authoring/gradient-64x32.png").to_vec()
    }

    /// The helper hardens itself, then tries calls a picture decoder must never make.
    #[test]
    #[ignore = "requires the built helper (cargo build --bins)"]
    fn the_helper_filter_really_refuses_dangerous_calls_but_not_ordinary_ones() {
        let helper = helper().expect("built helper");
        let output = Command::new(helper).arg("selftest").output().unwrap();
        let report = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "the filter let something through:\n{report}");
        for call in ["socket", "io_uring_setup", "bpf", "keyctl", "userfaultfd", "perf_event_open", "execve", "mount"] {
            assert!(report.contains(&format!("{call}: refused")), "{call} must be refused:\n{report}");
        }
        assert!(report.contains("ordinary calls and threads: work"), "{report}");
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn crop_rotate_and_scale_run_in_the_real_sandbox() {
        let (_dir, source) = fixture("photo.png", &gradient_png());
        let info = probe(&source, NEVER).unwrap();
        assert_eq!((info.width, info.height, info.kind), (64, 32, Kind::Image));
        let work = tempdir().unwrap();
        let job = Job {
            crop: Some(Rect { x: 16, y: 8, width: 32, height: 16 }),
            rotation: Rotation::Clockwise90,
            max_edge: Some(8),
            ..Default::default()
        };
        let made = author(&source, &job, &work.0, NEVER).unwrap();
        // 32x16 cropped, turned to 16x32, then scaled so the long edge is 8.
        assert_eq!((made.width, made.height, made.format), (4, 8, Format::Png));
        assert!(made.path.starts_with(&work.0) && made.path.is_file());
        let leftovers: Vec<_> = std::fs::read_dir(&work.0).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "scratch directory removed, only the result remains");
        assert!(std::fs::read(&source).unwrap() == gradient_png(), "the original is untouched");
    }

    fn pixel_at(decoded: &(usize, usize, usize, Vec<u8>), x: usize, y: usize) -> [u8; 3] {
        let (width, _, channels, pixels) = decoded;
        let at = (y * width + x) * channels;
        [pixels[at], pixels[at + 1], pixels[at + 2]]
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn a_square_is_drawn_by_the_helper_and_nothing_else_changes() {
        let (_dir, source) = fixture("photo.png", &gradient_png());
        let original = decode_png(&gradient_png());
        let work = tempdir().unwrap();
        let job = Job {
            marks: vec![Mark::Square { rect: Rect { x: 8, y: 4, width: 20, height: 12 }, color: [255, 0, 0], stroke: 2 }],
            ..Default::default()
        };
        let made = author(&source, &job, &work.0, NEVER).unwrap();
        assert_eq!((made.width, made.height), (64, 32));
        let marked = decode_png(&std::fs::read(&made.path).unwrap());
        for (x, y) in [(8, 4), (27, 4), (8, 15), (27, 15), (9, 5), (17, 4), (17, 5), (8, 9), (27, 9)] {
            assert_eq!(pixel_at(&marked, x, y), [255, 0, 0], "({x},{y}) is on the outline");
        }
        for (x, y) in [(17, 9), (12, 8), (7, 4), (28, 4), (8, 3), (8, 16), (0, 0), (63, 31), (40, 20)] {
            assert_eq!(pixel_at(&marked, x, y), pixel_at(&original, x, y), "({x},{y}) is untouched");
        }
        assert!(std::fs::read(&source).unwrap() == gradient_png(), "the original is untouched");
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn marks_follow_the_crop_and_the_rotation() {
        let (_dir, source) = fixture("photo.png", &gradient_png());
        let work = tempdir().unwrap();
        let job = Job {
            crop: Some(Rect { x: 4, y: 2, width: 40, height: 20 }),
            rotation: Rotation::Clockwise90,
            // In source pixels: its top-left corner is (4, 2) inside the crop.
            marks: vec![Mark::Square { rect: Rect { x: 8, y: 4, width: 20, height: 12 }, color: [0, 0, 255], stroke: 2 }],
            ..Default::default()
        };
        let made = author(&source, &job, &work.0, NEVER).unwrap();
        assert_eq!((made.width, made.height), (20, 40), "40x20 turned a quarter");
        let marked = decode_png(&std::fs::read(&made.path).unwrap());
        // Crop-local (x, y) goes to (20 - 1 - y, x) after a clockwise quarter turn.
        let turned = |x: usize, y: usize| (19 - y, x);
        for (x, y) in [(4, 2), (23, 2), (4, 13), (23, 13), (10, 2), (4, 8)] {
            let (tx, ty) = turned(x, y);
            assert_eq!(pixel_at(&marked, tx, ty), [0, 0, 255], "crop-local ({x},{y}) on the outline");
        }
        let (cx, cy) = turned(14, 8);
        assert_ne!(pixel_at(&marked, cx, cy), [0, 0, 255], "the inside of the square is not painted");
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn circles_arrows_and_freehand_lines_are_drawn_by_the_helper() {
        let (_dir, source) = fixture("photo.png", &gradient_png());
        let original = decode_png(&gradient_png());
        let work = tempdir().unwrap();
        let job = Job {
            marks: vec![
                Mark::Circle { rect: Rect { x: 4, y: 2, width: 20, height: 20 }, color: [0, 255, 0], stroke: 2 },
                Mark::Arrow { from: (30, 6), to: (60, 6), color: [0, 0, 255], stroke: 2 },
                Mark::Freehand { points: vec![(30, 20), (60, 20), (60, 30)], color: [255, 255, 0], stroke: 2, pressure: vec![] },
            ],
            ..Default::default()
        };
        let made = author(&source, &job, &work.0, NEVER).unwrap();
        let marked = decode_png(&std::fs::read(&made.path).unwrap());
        let painted = |points: &[(usize, usize)], colour: [u8; 3], what: &str| {
            for (x, y) in points {
                assert_eq!(pixel_at(&marked, *x, *y), colour, "{what} at ({x},{y})");
            }
        };
        painted(&[(13, 2), (14, 2), (4, 11), (23, 11), (13, 21)], [0, 255, 0], "the ring");
        painted(&[(31, 6), (45, 6), (59, 6), (60, 6), (54, 9), (54, 3)], [0, 0, 255], "the arrow");
        painted(&[(30, 20), (45, 20), (60, 20), (60, 25), (60, 30)], [255, 255, 0], "the line");
        for (x, y) in [(14, 12), (45, 12), (0, 0), (20, 28), (63, 0), (45, 26), (40, 31)] {
            assert_eq!(pixel_at(&marked, x, y), pixel_at(&original, x, y), "({x},{y}) is untouched");
        }
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn arrows_and_lines_follow_the_crop_and_the_rotation() {
        let (_dir, source) = fixture("photo.png", &gradient_png());
        let work = tempdir().unwrap();
        let job = Job {
            crop: Some(Rect { x: 4, y: 2, width: 40, height: 20 }),
            rotation: Rotation::Clockwise90,
            marks: vec![
                // Crop-local: a horizontal arrow along y = 8 from x = 6 to x = 30.
                Mark::Arrow { from: (10, 10), to: (34, 10), color: [0, 0, 255], stroke: 2 },
                Mark::Freehand { points: vec![(10, 18), (34, 18)], color: [255, 255, 0], stroke: 2, pressure: vec![] },
            ],
            ..Default::default()
        };
        let made = author(&source, &job, &work.0, NEVER).unwrap();
        assert_eq!((made.width, made.height), (20, 40));
        let marked = decode_png(&std::fs::read(&made.path).unwrap());
        // Crop-local (x, y) goes to (19 - y, x) after a clockwise quarter turn, so the horizontal
        // arrow (y = 8) became a vertical one at x = 11 and the line (y = 16) one at x = 3.
        for y in [8, 14, 20, 28] {
            assert_eq!(pixel_at(&marked, 11, y), [0, 0, 255], "arrow shaft at row {y}");
        }
        for y in [6, 14, 24, 30] {
            assert_eq!(pixel_at(&marked, 3, y), [255, 255, 0], "line at row {y}");
        }
        // Between the two lines, four pixels from each, nothing is drawn.
        let between = pixel_at(&marked, 7, 10);
        assert!(between != [0, 0, 255] && between != [255, 255, 0], "{between:?}");
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn a_stroke_outside_the_picture_is_refused_before_the_sandbox_runs() {
        let (_dir, source) = fixture("photo.png", &gradient_png());
        let work = tempdir().unwrap();
        for mark in [
            Mark::Arrow { from: (0, 0), to: (65, 10), color: [0, 0, 0], stroke: 2 },
            Mark::Freehand { points: vec![(1, 1), (10, 33)], color: [0, 0, 0], stroke: 2, pressure: vec![] },
            Mark::Freehand { points: vec![], color: [0, 0, 0], stroke: 2, pressure: vec![] },
            Mark::Circle { rect: Rect { x: 50, y: 0, width: 20, height: 20 }, color: [0, 0, 0], stroke: 2 },
        ] {
            let job = Job { marks: vec![mark], ..Default::default() };
            assert!(author(&source, &job, &work.0, NEVER).is_err());
        }
        assert_eq!(std::fs::read_dir(&work.0).unwrap().count(), 0, "nothing was written");
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn a_mark_outside_the_picture_is_refused_before_the_sandbox_runs() {
        let (_dir, source) = fixture("photo.png", &gradient_png());
        let work = tempdir().unwrap();
        let job = Job {
            marks: vec![Mark::Square { rect: Rect { x: 60, y: 0, width: 20, height: 12 }, color: [255, 0, 0], stroke: 2 }],
            ..Default::default()
        };
        assert!(author(&source, &job, &work.0, NEVER).is_err());
        assert_eq!(std::fs::read_dir(&work.0).unwrap().count(), 0, "nothing was written");
    }

    /// Minimal PNG reader for 8-bit RGB/RGBA, non-interlaced files (what the helper writes), so
    /// tests can check that the right pixels ended up in the right places.
    fn decode_png(bytes: &[u8]) -> (usize, usize, usize, Vec<u8>) {
        let (width, height) = (
            u32::from_be_bytes(bytes[16..20].try_into().unwrap()) as usize,
            u32::from_be_bytes(bytes[20..24].try_into().unwrap()) as usize,
        );
        assert_eq!((bytes[24], bytes[28]), (8, 0), "8-bit, not interlaced");
        let channels = match bytes[25] {
            2 => 3,
            6 => 4,
            other => panic!("unexpected colour type {other}"),
        };
        let (mut at, mut data) = (8, Vec::new());
        while at + 8 <= bytes.len() {
            let length = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            if &bytes[at + 4..at + 8] == b"IDAT" {
                data.extend_from_slice(&bytes[at + 8..at + 8 + length]);
            }
            at += 12 + length;
        }
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&data[..]).read_to_end(&mut raw).unwrap();
        let stride = width * channels;
        let mut pixels = vec![0u8; stride * height];
        for y in 0..height {
            let filter = raw[y * (stride + 1)];
            for i in 0..stride {
                let value = raw[y * (stride + 1) + 1 + i];
                let left = if i >= channels { pixels[y * stride + i - channels] } else { 0 };
                let up = if y > 0 { pixels[(y - 1) * stride + i] } else { 0 };
                let corner = if y > 0 && i >= channels { pixels[(y - 1) * stride + i - channels] } else { 0 };
                let predicted = match filter {
                    0 => 0,
                    1 => left,
                    2 => up,
                    3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                    4 => {
                        let (a, b, c) = (i32::from(left), i32::from(up), i32::from(corner));
                        let p = a + b - c;
                        let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                        (if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c }) as u8
                    }
                    other => panic!("filter {other}"),
                };
                pixels[y * stride + i] = value.wrapping_add(predicted);
            }
        }
        (width, height, channels, pixels)
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn crop_and_rotation_move_the_right_pixels() {
        let (_dir, source) = fixture("photo.png", &gradient_png());
        let work = tempdir().unwrap();
        let job = Job {
            crop: Some(Rect { x: 16, y: 8, width: 32, height: 16 }),
            rotation: Rotation::Clockwise90,
            ..Default::default()
        };
        let made = author(&source, &job, &work.0, NEVER).unwrap();
        assert_eq!((made.width, made.height), (16, 32));
        let (width, height, channels, pixels) = decode_png(&std::fs::read(&made.path).unwrap());
        assert_eq!((width, height), (16, 32));
        // The source is the gradient (x*4, y*8, (x+y)*2). Clockwise: out(ox, oy) = crop(oy, 15-ox).
        for (ox, oy) in [(0, 0), (15, 0), (0, 31), (15, 31), (7, 12)] {
            let (sx, sy) = (16 + oy, 8 + (15 - ox));
            let expected = [(sx * 4 % 256) as u8, (sy * 8 % 256) as u8, ((sx + sy) * 2 % 256) as u8];
            let at = (oy * width + ox) * channels;
            assert_eq!(pixels[at..at + 3], expected, "pixel ({ox}, {oy}) comes from source ({sx}, {sy})");
        }
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn jpeg_keeps_its_format_and_exif_orientation_and_webp_becomes_png() {
        let work = tempdir().unwrap();
        let plain = include_bytes!("../tests/fixtures/authoring/smpte-64x32.jpg");
        let turned = include_bytes!("../tests/fixtures/authoring/smpte-exif90-64x32.jpg");
        let webp = include_bytes!("../tests/fixtures/authoring/gradient-64x32.webp");
        let edit = |name: &str, bytes: &[u8]| {
            let (_dir, source) = fixture(name, bytes);
            let info = probe(&source, NEVER).unwrap();
            let made = author(&source, &Job::default(), &work.0, NEVER).unwrap();
            (info, made)
        };
        let (info, made) = edit("a.jpg", plain);
        assert_eq!((info.width, info.height, made.format), (64, 32, Format::Jpeg));
        // The phone-style orientation tag is applied: the file is stored 64x32 but shown 32x64.
        let (info, made) = edit("b.jpg", turned);
        assert_eq!((info.width, info.height), (32, 64));
        assert_eq!((made.width, made.height, made.format), (32, 64, Format::Jpeg));
        let (info, made) = edit("c.webp", webp);
        assert_eq!((info.width, info.height), (64, 32));
        assert_eq!((made.format, made.path.extension().unwrap().to_str().unwrap()), (Format::Png, "png"));
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn hostile_files_are_refused_or_fail_without_hanging() {
        let started = Instant::now();
        let png_claiming = |width: u32, height: u32| {
            let mut bytes = gradient_png();
            bytes[16..20].copy_from_slice(&width.to_be_bytes());
            bytes[20..24].copy_from_slice(&height.to_be_bytes());
            bytes
        };
        for (name, bytes) in [
            ("text.png", b"hello, not a picture".to_vec()),
            ("anim.gif", b"GIF89a\x01\x00\x01\x00\x00\x00\x00;".to_vec()),
            ("huge.png", png_claiming(60_000, 60_000)),
            ("zero.png", png_claiming(0, 10)),
            ("truncated.png", gradient_png()[..60].to_vec()),
            ("empty.png", Vec::new()),
        ] {
            let (_dir, source) = fixture(name, &bytes);
            let work = tempdir().unwrap();
            let result = author(&source, &Job::default(), &work.0, NEVER);
            assert!(result.is_err(), "{name} must not be edited");
            assert_eq!(std::fs::read_dir(&work.0).unwrap().count(), 0, "{name} leaves nothing behind");
        }
        assert!(started.elapsed() < Duration::from_secs(60));
    }

    #[test]
    #[ignore = "requires the built helper, GStreamer plugins and real bubblewrap namespaces"]
    fn editing_can_be_cancelled_and_missing_sources_are_unavailable() {
        let (_dir, source) = fixture("photo.png", &gradient_png());
        let work = tempdir().unwrap();
        assert_eq!(
            author(&source, &Job::default(), &work.0, &|| true).unwrap_err(),
            Error::Cancelled
        );
        assert_eq!(
            probe(Path::new("/nonexistent/photo.png"), NEVER).unwrap_err(),
            Error::Unavailable
        );
    }
}
