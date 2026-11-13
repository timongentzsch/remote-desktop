//! Builds x264 from the pinned sources in x264/ and links it statically, so the binary carries its encoder
//! instead of depending on whichever x264 version a distribution ships. Needs nasm when targeting x86.
//! The GPU encoder needs only the headers in nv-codec-headers/, and receiving the screen casts of GNOME and
//! KDE only those in pipewire/; the libraries of both are loaded at run time.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// A command as one string of words, the form x264's configure script takes its tools in.
fn words(command: &Command) -> OsString {
    let mut joined = command.get_program().to_os_string();
    for argument in command.get_args() {
        joined.push(" ");
        joined.push(argument);
    }
    joined
}

/// The C sources: the encoders behind stream/encode.rs, and on Linux PipeWire's stream behind
/// platform/linux/capture/pipewire.rs.
const ENCODERS: [&str; 7] = [
    "src/stream/encoder.c",
    "src/stream/x264.c",
    "src/stream/nvenc.c",
    "src/stream/vaapi.c",
    "src/stream/vaapi_h264.c",
    "src/stream/vaapi_hevc.c",
    "src/stream/nal.c",
];
/// libva's headers, of which va/va_version.h is made from its template, with the version libva's build gives it.
const LIBVA: &str = "libva";
const PIPEWIRE: &str = "src/platform/linux/capture/pipewire_stream.c";

/// Makes va/va_version.h from libva's template and the VA-API version in its meson.build; returns the directory
/// to add to the include path for it.
fn libva_version() -> PathBuf {
    let meson = std::fs::read_to_string(format!("{LIBVA}/meson.build")).expect("libva/ is checked out");
    let number = |name: &str| {
        let line = meson.lines().find(|line| line.trim_start().starts_with(name)).expect("a VA-API version");
        line.split('=').nth(1).unwrap().trim().to_owned()
    };
    let (major, minor, micro) =
        (number("va_api_major_version"), number("va_api_minor_version"), number("va_api_micro_version"));
    let header = std::fs::read_to_string(format!("{LIBVA}/va/va_version.h.in"))
        .unwrap()
        .replace("@VA_API_MAJOR_VERSION@", &major)
        .replace("@VA_API_MINOR_VERSION@", &minor)
        .replace("@VA_API_MICRO_VERSION@", &micro)
        .replace("@VA_API_VERSION@", &format!("{major}.{minor}.{micro}"));
    let directory = PathBuf::from(env::var("OUT_DIR").unwrap()).join("libva");
    std::fs::create_dir_all(directory.join("va")).unwrap();
    std::fs::write(directory.join("va/va_version.h"), header).unwrap();
    directory
}

fn run(command: &mut Command) {
    let status = command.status().unwrap_or_else(|error| panic!("cannot run {command:?}: {error}"));
    assert!(status.success(), "{command:?} failed");
}

fn main() {
    for file in ENCODERS.iter().chain(&[PIPEWIRE, "src/stream/encoder.h", "src/stream/nal.h", "src/stream/vaapi.h"]) {
        println!("cargo:rerun-if-changed={file}");
    }
    println!("cargo:rerun-if-changed=x264/x264.h");
    let source = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("x264");
    let build = PathBuf::from(env::var("OUT_DIR").unwrap()).join("x264");
    let tools = cc::Build::new();

    if !build.join("libx264.a").exists() {
        std::fs::create_dir_all(&build).unwrap();
        // The compiler and archiver cargo picked for the target, so cross builds configure x264 the same way.
        run(Command::new(source.join("configure"))
            .current_dir(&build)
            .args(["--enable-static", "--enable-pic", "--disable-cli", "--disable-opencl", "--bit-depth=8"])
            .arg(format!("--host={}-linux", env::var("CARGO_CFG_TARGET_ARCH").unwrap()))
            .env("CC", words(&tools.get_compiler().to_command()))
            .env("AR", words(&tools.get_archiver()))
            .env("RANLIB", words(&tools.get_ranlib())));
        run(Command::new("make")
            .current_dir(&build)
            .arg(format!("-j{}", env::var("NUM_JOBS").unwrap()))
            .arg("libx264.a"));
    }

    cc::Build::new()
        .files(ENCODERS)
        .include(&source)
        .include(&build)
        .include("nv-codec-headers/include")
        .include(LIBVA)
        .include(libva_version())
        .compile("encoder");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        cc::Build::new()
            .file(PIPEWIRE)
            .include("pipewire/src")
            .include("pipewire/spa/include")
            .compile("pipewire_stream");
    }
    println!("cargo:rustc-link-search=native={}", build.display());
    println!("cargo:rustc-link-lib=static=x264");
}
