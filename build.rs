//! Links sherpa-onnx's C API statically for `sopen stems` (spec §7a) when the `stems` feature is
//! on (the default): the prebuilt static archive of sherpa-onnx v2.0.2 for the *target* (not
//! the host — the x86_64 macOS binary is built on an arm64 runner), downloaded once into
//! `OUT_DIR` and checked against its pinned SHA-256 before it is unpacked. ONNX Runtime comes in
//! the same archive. `SHERPA_ONNX_LIB_DIR` names an already extracted `lib/` instead (offline
//! builds; not checked). The link directives are sherpa-onnx-sys's own (its build.rs,
//! `emit_static_link_directives`).

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION: &str = "2.0.2";

/// `(target os, target arch, the release's platform name, SHA-256 of
/// sherpa-onnx-v<VERSION>-<platform>-static-lib.tar.bz2)`.
const ARCHIVES: [(&str, &str, &str, &str); 4] = [
    (
        "macos",
        "aarch64",
        "osx-arm64",
        "3e770a0062e9d0b7c16ca81c15098f08188540906a82a0f213ce3d8ee34b659c",
    ),
    (
        "macos",
        "x86_64",
        "osx-x64",
        "2515789c0013fab2d1f080430dc098137112a5ec4dcf63a5619be6c0adf0189d",
    ),
    (
        "linux",
        "x86_64",
        "linux-x64",
        "082743c80ec9d3fd739df174b9e46ab08ac77c9f57a1d2b09977a7fd5324727d",
    ),
    (
        "linux",
        "aarch64",
        "linux-aarch64",
        "c2ff3464e4065b4e04a0cdbc00f43f4e44405d6c3ea4e4023f4a4e069645e3fb",
    ),
];

/// The archive's libraries, in link order (sherpa-onnx-sys's `SHERPA_ONNX_STATIC_LIBS`). The
/// linker takes from each only what source separation reaches.
const LIBS: [&str; 13] = [
    "sherpa-onnx-c-api",
    "sherpa-onnx-core",
    "kaldi-hmm-gmm-core",
    "kaldi-native-io-core",
    "kaldi-decoder-core",
    "sherpa-onnx-kaldifst-core",
    "sherpa-onnx-fstfar",
    "sherpa-onnx-fst",
    "kaldi-native-fbank-core",
    "kissfft-float",
    "onnxruntime",
    "ssentencepiece_core",
    "utf8proc",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SHERPA_ONNX_LIB_DIR");
    if std::env::var_os("CARGO_FEATURE_STEMS").is_none() {
        return;
    }
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let lib_dir = match std::env::var_os("SHERPA_ONNX_LIB_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => fetch(&os, &arch).unwrap_or_else(|e| fail(&e)),
    };
    for lib in LIBS {
        let file = lib_dir.join(format!("lib{lib}.a"));
        if !file.is_file() {
            fail(&format!("{} is missing", file.display()));
        }
    }
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    for lib in LIBS {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    match os.as_str() {
        "linux" => {
            for lib in ["stdc++", "m", "pthread", "dl"] {
                println!("cargo:rustc-link-lib=dylib={lib}");
            }
        }
        "macos" => {
            println!("cargo:rustc-link-lib=dylib=c++");
            println!("cargo:rustc-link-lib=framework=Foundation");
        }
        _ => {}
    }
}

fn fail(why: &str) -> ! {
    panic!(
        "sopen's `stems` feature (voice / music separation) links sherpa-onnx v{VERSION}: {why}. \
         For an offline build set SHERPA_ONNX_LIB_DIR to an extracted archive's lib/ \
         (https://github.com/k2-fsa/sherpa-onnx/releases/tag/v{VERSION}, \
         sherpa-onnx-v{VERSION}-<platform>-static-lib.tar.bz2); or build without it: \
         cargo build --no-default-features"
    )
}

/// The extracted `lib/` for this target under `OUT_DIR`, downloaded and checked first when it
/// is not there yet.
fn fetch(os: &str, arch: &str) -> Result<PathBuf, String> {
    let (platform, sha) = ARCHIVES
        .iter()
        .find(|(o, a, _, _)| *o == os && *a == arch)
        .map(|(_, _, platform, sha)| (*platform, *sha))
        .ok_or_else(|| format!("no prebuilt archive for {os}/{arch}"))?;
    let out = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| format!("OUT_DIR: {e}"))?);
    let stem = format!("sherpa-onnx-v{VERSION}-{platform}-static-lib");
    let lib_dir = out.join(&stem).join("lib");
    // Written last: an extraction cut short is never taken for a finished one.
    let stamp = out.join(format!("{stem}.sha256"));
    if std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == sha) && lib_dir.is_dir() {
        return Ok(lib_dir);
    }
    let archive = out.join(format!("{stem}.tar.bz2"));
    let url = format!(
        "https://github.com/k2-fsa/sherpa-onnx/releases/download/v{VERSION}/{stem}.tar.bz2"
    );
    let part = out.join(format!("{stem}.tar.bz2.part"));
    run(Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--retry",
            "3",
            "--connect-timeout",
            "30",
        ])
        .arg("-o")
        .arg(&part)
        .arg(&url))
    .map_err(|e| format!("download of {url} failed: {e}"))?;
    let got = sha256(&part)?;
    if got != sha {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{url} has SHA-256 {got}, not the pinned {sha}"));
    }
    std::fs::rename(&part, &archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let _ = std::fs::remove_dir_all(out.join(&stem));
    run(Command::new("tar")
        .arg("-xjf")
        .arg(&archive)
        .arg("-C")
        .arg(&out))
    .map_err(|e| format!("cannot unpack {}: {e}", archive.display()))?;
    let _ = std::fs::remove_file(&archive);
    std::fs::write(&stamp, sha).map_err(|e| format!("{}: {e}", stamp.display()))?;
    Ok(lib_dir)
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(status.to_string())
    }
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
