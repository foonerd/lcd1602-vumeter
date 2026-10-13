//! Link against the ALSA library of the target: the one the ship script
//! unpacked into the sysroot for a cross build, else the system's.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target = env::var("TARGET").unwrap_or_default();
    let (deb, multiarch) = match target.as_str() {
        "x86_64-unknown-linux-gnu" => ("amd64", "x86_64-linux-gnu"),
        "armv7-unknown-linux-gnueabihf" => ("armhf", "arm-linux-gnueabihf"),
        "aarch64-unknown-linux-gnu" => ("arm64", "aarch64-linux-gnu"),
        _ => ("", ""),
    };
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let mut candidates = Vec::new();
    if !deb.is_empty() {
        let unpacked = root
            .join("target/sysroot")
            .join(deb)
            .join("usr/lib")
            .join(multiarch)
            .join("libasound.so.2");
        // Run again when the unpacked library appears, changes or goes, so
        // a build never links by a choice made for another sysroot.
        println!("cargo:rerun-if-changed={}", unpacked.display());
        candidates.push(unpacked);
        candidates.push(
            PathBuf::from("/usr/lib")
                .join(multiarch)
                .join("libasound.so.2"),
        );
        candidates.push(PathBuf::from("/lib").join(multiarch).join("libasound.so.2"));
    }
    for candidate in candidates {
        if candidate.exists() {
            println!("cargo:rustc-link-arg={}", candidate.display());
            return;
        }
    }
    println!("cargo:rustc-link-lib=dylib=asound");
}
