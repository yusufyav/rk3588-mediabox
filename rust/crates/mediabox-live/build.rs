//! The audio sink's fields the sound clock's controller reads
//! (src/gst_abi.c), compiled against the GStreamer headers the player
//! links -- the compiler, not a hand-written offset, places them -- and
//! the GNU build-ids of the libraries those headers came with, for the
//! run-time guard (src/gst_abi.rs) to hold the loaded ones to.

use std::{env, path::PathBuf, process::Command};

#[path = "src/elf_note.rs"]
mod elf_note;

/// A shared object's GNU build-id, from its PT_NOTE program headers.
fn build_id_of(path: &std::path::Path) -> String {
    let elf = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(elf.starts_with(b"\x7fELF\x02\x01"), "{}: not a little-endian ELF64", path.display());
    let u16_at = |at: usize| u16::from_le_bytes(elf[at..at + 2].try_into().unwrap()) as usize;
    let u64_at = |at: usize| u64::from_le_bytes(elf[at..at + 8].try_into().unwrap()) as usize;
    let (phoff, phentsize, phnum) = (u64_at(0x20), u16_at(0x36), u16_at(0x38));
    (0..phnum)
        .map(|i| phoff + i * phentsize)
        .filter(|&ph| u32::from_le_bytes(elf[ph..ph + 4].try_into().unwrap()) == 4)
        .find_map(|ph| elf_note::build_id(&elf[u64_at(ph + 8)..u64_at(ph + 8) + u64_at(ph + 32)]))
        .unwrap_or_else(|| panic!("{}: no GNU build-id", path.display()))
}

fn main() {
    println!("cargo:rerun-if-changed=src/gst_abi.c");
    println!("cargo:rerun-if-changed=src/elf_note.rs");
    for var in ["PKG_CONFIG_SYSROOT_DIR", "PKG_CONFIG_LIBDIR", "PKG_CONFIG_PATH"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    let audio = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("gstreamer-audio-1.0")
        .expect("gstreamer-audio-1.0 by pkg-config");

    let target = env::var("TARGET").unwrap();
    let host = env::var("HOST").unwrap();
    let cc = env::var(format!("CC_{}", target.replace('-', "_"))).unwrap_or_else(|_| {
        if target == host {
            "cc".into()
        } else if target == "aarch64-unknown-linux-gnu" {
            "aarch64-linux-gnu-gcc".into()
        } else {
            panic!("no C compiler known for {target}: set CC_{}", target.replace('-', "_"))
        }
    });
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let object = out.join("gst_abi.o");
    let mut compile = Command::new(&cc);
    // From src/, by a relative name: no build path in the object.
    compile.current_dir(PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("src"));
    compile.args(["-c", "-O2", "-fPIC", "-Wall", "-Werror", "-o"]).arg(&object).arg("gst_abi.c");
    if let Ok(sysroot) = env::var("PKG_CONFIG_SYSROOT_DIR") {
        compile.arg(format!("--sysroot={sysroot}"));
    }
    for dir in &audio.include_paths {
        compile.arg("-I").arg(dir);
    }
    let status = compile.status().unwrap_or_else(|e| panic!("{cc}: {e}"));
    assert!(status.success(), "{cc} gst_abi.c: {status}");
    println!("cargo:rustc-link-arg={}", object.display());
    for dir in &audio.link_paths {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    println!("cargo:rustc-link-arg=-lgstaudio-1.0");

    // The libraries these headers came with: libgstreamer for the clock,
    // libgstaudio for the sink.
    for (lib, var) in [("libgstreamer-1.0.so", "MEDIABOX_GST_CORE_BUILD_ID"), ("libgstaudio-1.0.so", "MEDIABOX_GST_AUDIO_BUILD_ID")] {
        let path = audio
            .link_paths
            .iter()
            .map(|d| d.join(lib))
            .find(|p| p.exists())
            .unwrap_or_else(|| panic!("{lib} in none of {:?}", audio.link_paths));
        let real = path.canonicalize().unwrap();
        println!("cargo:rerun-if-changed={}", real.display());
        println!("cargo:rustc-env={var}={}", build_id_of(&real));
    }
}
