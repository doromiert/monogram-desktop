fn main() {
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
}
