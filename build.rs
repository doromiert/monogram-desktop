/// Embed the git commit this client was built from, for `/neofetch`. Builds without a git
/// checkout (a source tarball, Nix) can pass `MONOGRAM_GIT_COMMIT` (and `MONOGRAM_GIT_DIRTY=1`);
/// otherwise the commit is simply left out.
fn git_commit() {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    println!("cargo:rerun-if-env-changed=MONOGRAM_GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=MONOGRAM_GIT_DIRTY");
    let commit = std::env::var("MONOGRAM_GIT_COMMIT")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| git(&["rev-parse", "--short=7", "HEAD"]))
        .filter(|value| value.chars().all(|c| c.is_ascii_alphanumeric()));
    if let Some(commit) = commit {
        println!("cargo:rustc-env=MONOGRAM_GIT_COMMIT={commit}");
        let dirty = std::env::var("MONOGRAM_GIT_DIRTY").ok().map(|value| value == "1").unwrap_or_else(|| {
            git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|status| !status.is_empty())
        });
        println!("cargo:rustc-env=MONOGRAM_GIT_DIRTY={}", u8::from(dirty));
    }
    // Every commit and checkout appends to the reflog; the index changes when files are staged.
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/logs/HEAD");
        println!("cargo:rerun-if-changed={dir}/index");
    }
}
fn main() {
    git_commit();
    println!("cargo:rerun-if-env-changed=MONOGRAM_MATH_HELPER");
    println!("cargo:rustc-env=MONOGRAM_BUILT_MATH_HELPER={}", std::env::var("MONOGRAM_MATH_HELPER").unwrap_or_default());
    println!("cargo:rerun-if-env-changed=MONOGRAM_BWRAP");
    let bwrap = std::env::var_os("MONOGRAM_BWRAP").map(std::path::PathBuf::from).or_else(|| std::env::var_os("PATH").and_then(|paths|std::env::split_paths(&paths).map(|path|path.join("bwrap")).find(|path|path.is_file()))).expect("bubblewrap");
    println!("cargo:rustc-env=MONOGRAM_BUILT_BWRAP={}", bwrap.display());
    let backend = pkg_config::Config::new()
        .cargo_metadata(false)
        .atleast_version("0.5.1")
        .probe("libomemo-c")
        .expect("libomemo-c 0.5.1 (same source as mobile)");
    let ssl = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("openssl")
        .expect("OpenSSL");
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .opt_level(2)
        .std("c++17")
        .file("native/omemo.cpp");
    for path in backend.include_paths.iter().chain(&ssl.include_paths) {
        build.include(path);
    }
    build
        .flag_if_supported("-Wno-unused-parameter")
        .compile("monogram_omemo_adapter");
    let mut provider = cc::Build::new();
    provider.opt_level(2).file("native/crypto_provider.c");
    for path in backend.include_paths.iter().chain(&ssl.include_paths) {
        provider.include(path);
    }
    provider
        .flag_if_supported("-Wno-deprecated-declarations")
        .flag_if_supported("-Wno-unused-parameter")
        .compile("monogram_omemo_provider");
    for path in backend.link_paths.iter().chain(&ssl.link_paths) {
        println!("cargo:rustc-link-search=native={}", path.display());
    }
    for library in backend.libs.iter().chain(&ssl.libs) {
        println!("cargo:rustc-link-lib={library}");
    }
    println!("cargo:rerun-if-changed=native/omemo.cpp");
    println!("cargo:rerun-if-changed=native/crypto_provider.c");
    let image = pkg_config::Config::new()
        .cargo_metadata(false)
        .atleast_version("2.1")
        .probe("glycin-gtk4-2")
        .expect("libglycin-gtk4 2.1 or newer");
    let unix = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("gio-unix-2.0")
        .expect("GIO Unix streams");
    // The `hdr` feature is on by default. Without libultrahdr, the build warns and links a stub
    // that reports "no HDR", so ordinary stills take the normal decode path.
    let hdr = std::env::var_os("CARGO_FEATURE_HDR").is_some().then(|| {
        match pkg_config::Config::new().cargo_metadata(false).atleast_version("1.4").probe("libuhdr") {
            Ok(library) => Some(library),
            Err(_) => {
                println!("cargo:warning=building without HDR still images: libultrahdr 1.4 not found (pass --no-default-features to silence this warning)");
                None
            }
        }
    }).flatten();
    let (hdr_include, hdr_link, hdr_libs): (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>, Vec<String>) = match &hdr {
        Some(library) => (library.include_paths.clone(), library.link_paths.clone(), library.libs.clone()),
        None => Default::default(),
    };
    let mut bridge = cc::Build::new();
    bridge.opt_level(2).file("native/image_loader.c");
    bridge.file(if hdr.is_some() { "native/hdr_image.c" } else { "native/hdr_image_disabled.c" });
    for path in &hdr_include {bridge.include(path);}
    for path in image.include_paths.iter().chain(&unix.include_paths) {
        bridge.include(path);
    }
    bridge.compile("monogram_image_loader");
    // --as-needed requires the shared providers after the archive that references them.
    for path in image.link_paths.iter().chain(&unix.link_paths).chain(&hdr_link) {
        println!("cargo:rustc-link-search=native={}", path.display());
    }
    for library in image.libs.iter().chain(&unix.libs).chain(&hdr_libs) {
        println!("cargo:rustc-link-lib={library}");
    }
    println!("cargo:rerun-if-changed=native/image_loader.c");
    println!("cargo:rerun-if-changed=native/hdr_image.c");
    println!("cargo:rerun-if-changed=native/hdr_image_disabled.c");
    let app = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("gstreamer-app-1.0")
        .expect("GStreamer appsink");
    let raw_video = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("gstreamer-video-1.0")
        .expect("GStreamer video frames");
    let mut preview = cc::Build::new();
    preview.opt_level(2).file("native/image_preview.c");
    for paths in [
        &image.include_paths,
        &unix.include_paths,
        &app.include_paths,
        &raw_video.include_paths,
    ] {
        for path in paths {
            preview.include(path);
        }
    }
    preview.compile("monogram_link_preview");
    for dependency in [&image, &unix, &app, &raw_video] {
        for path in &dependency.link_paths {
            println!("cargo:rustc-link-search=native={}", path.display());
        }
        for library in &dependency.libs {
            println!("cargo:rustc-link-lib={library}");
        }
    }
    println!("cargo:rerun-if-changed=native/image_preview.c");
    let clapper = pkg_config::Config::new()
        .atleast_version("0.10.0")
        .probe("clapper-gtk-0.0")
        .expect("Clapper GTK 0.10 (same player as Galeria)");
    let mut video = cc::Build::new();
    video.opt_level(2).file("native/video_player.c");
    for path in &clapper.include_paths {
        video.include(path);
    }
    video
        .flag_if_supported("-Wno-unused-parameter")
        .compile("monogram_video_player");
    // Shared providers must follow the archive under --as-needed.
    for path in &clapper.link_paths {
        println!("cargo:rustc-link-search=native={}", path.display());
    }
    for library in &clapper.libs {
        println!("cargo:rustc-link-lib={library}");
    }
    println!("cargo:rerun-if-changed=native/video_player.c");
}
