use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rustc-link-search=framework=/System/Library/PrivateFrameworks");

    println!("cargo:rustc-link-lib=framework=SkyLight");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
    println!("cargo:rustc-link-lib=framework=CoreVideo");
    println!("cargo:rustc-link-lib=framework=IOKit");
    println!("cargo:rustc-link-lib=framework=MultitouchSupport");
    println!("cargo:rustc-link-lib=framework=Carbon");

    build_osax();
}

/// Builds the two binaries of the scripting addition and leaves them in
/// `OUT_DIR` for `sys::osax` to embed.
///
/// Both are fat x86_64 + arm64e, and arm64e is the point: the payload is
/// `dlopen`ed inside Dock, which is arm64e on Apple Silicon, and the loader
/// spawns a thread there, which needs the same pointer-authentication ABI.
/// Neither is built for the host triple, so `cc` is no help and clang is driven
/// directly.
fn build_osax() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/osax");
    for source in [
        "payload.m",
        "loader.m",
        "arm64_payload.m",
        "x64_payload.m",
        "common.h",
        "hashtable.h",
    ] {
        println!("cargo:rerun-if-changed={}", dir.join(source).display());
    }

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));

    clang(&dir.join("payload.m"), &out.join("payload"), &[
        "-shared",
        "-fPIC",
        "-F/System/Library/PrivateFrameworks",
        "-framework",
        "SkyLight",
        "-framework",
        "Foundation",
        "-framework",
        "Carbon",
    ]);

    clang(&dir.join("loader.m"), &out.join("loader"), &[
        "-framework",
        "Cocoa",
    ]);
}

fn clang(source: &Path, output: &Path, extra: &[&str]) {
    let mut command = Command::new("xcrun");
    command
        .arg("clang")
        .arg(source)
        .args(["-O3", "-mmacosx-version-min=11.0"])
        // -fno-objc-arc matches yabai's build: the vendored payload manages its
        // own retain/release and does not compile under ARC.
        .args(["-fno-objc-arc", "-arch", "x86_64", "-arch", "arm64e"])
        .args(extra)
        .arg("-o")
        .arg(output);

    let status = command.status().unwrap_or_else(|error| {
        panic!("failed to run xcrun clang for {}: {error}", source.display())
    });
    assert!(status.success(), "failed to build {}", source.display());
}
