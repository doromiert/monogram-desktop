fn main() {
    println!("cargo:rerun-if-env-changed=MONOGRAM_MATH_HELPER");
    println!("cargo:rustc-env=MONOGRAM_BUILT_MATH_HELPER={}", std::env::var("MONOGRAM_MATH_HELPER").unwrap_or_default());
    let helper = if let Some(helper) = std::env::var_os("MONOGRAM_PREBUILT_LOTTIE_HELPER") {
        let helper = std::path::PathBuf::from(helper);
        assert!(helper.is_file(), "prebuilt sticker renderer must exist");
        helper
    } else {
        let lottie = pkg_config::Config::new().cargo_metadata(false).atleast_version("0.2").probe("rlottie").expect("rlottie 0.2 C API");
        let zlib = pkg_config::Config::new().cargo_metadata(false).probe("zlib").expect("zlib");
        let helper = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("monogram-sticker-renderer");
        let mut command = cc::Build::new().get_compiler().to_command();
        command.args(["-O2", "-std=c11", "native/lottie_renderer.c", "-o"]).arg(&helper);
        for path in lottie.include_paths.iter().chain(&zlib.include_paths) { command.arg("-I").arg(path); }
        for path in lottie.link_paths.iter().chain(&zlib.link_paths) { command.arg("-L").arg(path).arg(format!("-Wl,-rpath,{}", path.display())); }
        for library in lottie.libs.iter().chain(&zlib.libs) { command.arg(format!("-l{library}")); }
        command.arg("-lm");
        assert!(command.status().expect("build sticker renderer").success(), "build rlottie sticker renderer");
        helper
    };
    println!("cargo:rustc-env=MONOGRAM_BUILT_LOTTIE_HELPER={}", helper.display());
    println!("cargo:rerun-if-env-changed=MONOGRAM_PREBUILT_LOTTIE_HELPER");
    println!("cargo:rerun-if-env-changed=MONOGRAM_BWRAP");
    println!("cargo:rerun-if-changed=native/lottie_renderer.c");
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
    let mut bridge = cc::Build::new();
    bridge.opt_level(2).file("native/image_loader.c");
    for path in image.include_paths.iter().chain(&unix.include_paths) {
        bridge.include(path);
    }
    bridge.compile("monogram_image_loader");
    // --as-needed requires the shared providers after the archive that references them.
    for path in image.link_paths.iter().chain(&unix.link_paths) {
        println!("cargo:rustc-link-search=native={}", path.display());
    }
    for library in image.libs.iter().chain(&unix.libs) {
        println!("cargo:rustc-link-lib={library}");
    }
    println!("cargo:rerun-if-changed=native/image_loader.c");
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
