//! Checks the system libraries and GStreamer plugins that Monogram needs at run time.
//!
//! These need no display. A missing plugin does not break the build; it makes media silently fail
//! in the app. For example, without `gtk4paintablesink` every video shows "Video playback
//! unavailable". The failure message names the package to install.
use gstreamer as gst;
use std::path::Path;

/// (element, Arch package, what breaks without it)
const REQUIRED_ELEMENTS: &[(&str, &str, &str)] = &[
    ("gtk4paintablesink", "gst-plugin-gtk4", "all video playback (\"Video playback unavailable\")"),
    ("playbin3", "gst-plugins-base", "video and audio playback"),
    ("decodebin", "gst-plugins-base", "video thumbnails and media authoring"),
    ("videoconvert", "gst-plugins-base", "video thumbnails and media authoring"),
    ("videoscale", "gst-plugins-base", "video thumbnails and media authoring"),
    ("appsink", "gst-plugins-base", "video thumbnails and audio previews"),
    ("fakesink", "gstreamer", "media probing"),
    ("filesrc", "gstreamer", "media probing"),
    ("filesink", "gstreamer", "media authoring output"),
    ("jpegdec", "gst-plugins-good", "JPEG upload preparation"),
    ("pngdec", "gst-plugins-good", "PNG upload preparation"),
    ("matroskademux", "gst-plugins-good", "WebM and MKV playback"),
    ("webpdec", "gst-plugins-bad", "WebP upload preparation"),
    ("vp9parse", "gst-plugins-bad", "VP9 sticker and video playback"),
    ("vp8dec", "gst-plugins-good", "VP8 sticker and video playback"),
    ("vp9dec", "gst-plugins-good", "VP9 sticker and video playback"),
    ("avdec_h264", "gst-libav", "H.264 (MP4) video playback"),
];

/// Features that only reduce functionality, so they warn instead of failing.
const OPTIONAL_ELEMENTS: &[(&str, &str, &str)] = &[("pipewiresrc", "gst-plugin-pipewire", "taking photos with the camera")];

fn missing(list: &[(&str, &str, &str)]) -> Vec<String> {
    gst::init().expect("GStreamer itself failed to initialise; install gstreamer");
    list.iter()
        .filter(|(name, _, _)| gst::ElementFactory::find(name).is_none())
        .map(|(name, package, effect)| format!("  - `{name}` is missing: install `{package}` (needed for {effect})"))
        .collect()
}

#[test]
fn required_gstreamer_elements_are_installed() {
    let missing = missing(REQUIRED_ELEMENTS);
    assert!(
        missing.is_empty(),
        "\nMonogram media will not work on this machine. Missing GStreamer elements:\n{}\n\
         After installing, clear the plugin cache: rm -rf ~/.cache/gstreamer-1.0\n",
        missing.join("\n")
    );
}

#[test]
fn optional_gstreamer_elements_warn_when_missing() {
    for line in missing(OPTIONAL_ELEMENTS) {
        eprintln!("warning: optional feature unavailable:\n{line}");
    }
}

#[test]
fn clapper_is_installed() {
    gst::init().expect("GStreamer failed to initialise");
    assert!(
        gst::ElementFactory::find("clappersink").is_some(),
        "\nThe Clapper GStreamer plugin is missing: install `libclapper` (video playback needs it).\n"
    );
}

#[test]
fn bubblewrap_is_installed() {
    let found = std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join("bwrap").is_file()));
    assert!(
        found,
        "\n`bwrap` was not found on PATH: install `bubblewrap`. Image decoding and the media helpers run inside it.\n"
    );
}

#[test]
fn glycin_image_loaders_are_installed() {
    let dirs = ["/usr/lib/glycin-loaders", "/usr/lib64/glycin-loaders", "/usr/libexec/glycin-loaders"];
    let found = dirs.iter().any(|dir| {
        std::fs::read_dir(Path::new(dir).join("2+"))
            .is_ok_and(|entries| entries.flatten().any(|entry| entry.file_name().to_string_lossy().starts_with("glycin-image-rs")))
    });
    assert!(
        found,
        "\nThe glycin image loaders were not found: install `glycin` (every photo preview uses them).\n"
    );
}

#[test]
fn libultrahdr_absence_is_reported() {
    // libultrahdr is optional (the `hdr` feature); say so instead of leaving it as a surprise.
    let has = std::process::Command::new("pkg-config")
        .args(["--atleast-version=1.4", "libuhdr"])
        .status()
        .is_ok_and(|status| status.success());
    if !has {
        eprintln!("warning: libultrahdr 1.4 not found; Ultra HDR JPEGs will be shown as ordinary images");
    }
}
