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
}
