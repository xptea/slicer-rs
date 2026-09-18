use std::path::PathBuf;
fn main() {
    println!("cargo:rerun-if-changed=native/decoder.c");
    println!("cargo:rerun-if-env-changed=SLICER_AV_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=SLICER_AV_LIB_DIR");
    if std::env::var_os("CARGO_FEATURE_DESKTOP").is_none() {
        return;
    }
    println!("cargo:rerun-if-changed=native/gl_canvas.c");
    cc::Build::new()
        .file("native/gl_canvas.c")
        .flag_if_supported("-std=c11")
        .compile("slicer_gl");
    for lib in ["libEGL.so.1", "libGL.so.1", "libX11.so.6"] {
        println!("cargo:rustc-link-lib=dylib:+verbatim={lib}");
    }
    let include = std::env::var_os("SLICER_AV_INCLUDE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let system = PathBuf::from("/usr/include/x86_64-linux-gnu");
            if system.join("libavcodec/avcodec.h").exists() {
                system
            } else {
                PathBuf::from("build/mpv-toolchain/sysroot/usr/include/x86_64-linux-gnu")
            }
        });
    if !include.join("libavcodec/avcodec.h").exists() {
        panic!("Install FFmpeg 8 development headers or set SLICER_AV_INCLUDE_DIR");
    }
    cc::Build::new()
        .file("native/decoder.c")
        .include(include)
        .flag_if_supported("-std=c11")
        .warnings(true)
        .compile("slicer_decoder");
    if let Some(lib) = std::env::var_os("SLICER_AV_LIB_DIR") {
        println!(
            "cargo:rustc-link-search=native={}",
            PathBuf::from(lib).display()
        );
    }
    for lib in [
        "avformat.so.62",
        "avcodec.so.62",
        "avutil.so.60",
        "swscale.so.9",
        "swresample.so.6",
    ] {
        println!("cargo:rustc-link-lib=dylib:+verbatim=lib{lib}");
    }
    println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags,-rpath,$ORIGIN/../lib");
}
