//! Build script: embed the Lumen icon into the Windows executable.
//!
//! Runs `windres` (the mingw resource compiler, already on PATH for the
//! `x86_64-pc-windows-gnu` toolchain) to compile a tiny `.rc` that references
//! `assets/icon.ico`, then links the resulting object into the binary. The icon
//! then shows in Explorer, the taskbar, and Alt-Tab for `lumen.exe`.
//!
//! This is a no-op on non-Windows targets and degrades gracefully: if `windres`
//! or the icon is missing, the build still succeeds (just without an icon) and a
//! `cargo:warning` explains why, so the compiler always builds.

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

fn main() {
    // Only Windows executables carry icon resources.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    // The icon lives at <repo>/assets/icon.ico; this script runs in <repo>/compiler.
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let icon = Path::new(&manifest_dir)
        .join("..")
        .join("assets")
        .join("icon.ico");
    if !icon.exists() {
        println!(
            "cargo:warning=icon not found at {} - building without an icon",
            icon.display()
        );
        return;
    }
    // Rebuild the resource if the icon changes.
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = env::var("OUT_DIR").unwrap_or_else(|_| ".".into());
    let rc_path = Path::new(&out_dir).join("lumen_icon.rc");
    let res_path = Path::new(&out_dir).join("lumen_icon.o");

    // A minimal resource script: icon ID 1 (the app/default icon). Use an
    // absolute, forward-slashed path so windres finds the .ico regardless of cwd.
    let icon_str = icon
        .canonicalize()
        .unwrap_or(icon.clone())
        .display()
        .to_string()
        .replace('\\', "/")
        // strip the Windows \\?\ verbatim prefix that canonicalize adds, which
        // windres does not understand.
        .replace("//?/", "");
    let rc = format!("1 ICON \"{icon_str}\"\n");
    if let Err(e) = fs::write(&rc_path, rc) {
        println!(
            "cargo:warning=could not write {} ({e}) - no icon",
            rc_path.display()
        );
        return;
    }

    // Compile the .rc into a COFF object with windres, then link it in.
    let windres = env::var("WINDRES").unwrap_or_else(|_| "windres".into());
    let status = Command::new(&windres)
        .args([
            "--input",
            &rc_path.display().to_string(),
            "--output",
            &res_path.display().to_string(),
            "--output-format=coff",
        ])
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-search=native={out_dir}");
            // link the object directly (it carries the .rsrc section)
            println!("cargo:rustc-link-arg={}", res_path.display());
        }
        Ok(s) => println!("cargo:warning=windres exited with {s} - building without an icon"),
        Err(e) => {
            println!("cargo:warning=could not run `{windres}` ({e}) - building without an icon")
        }
    }
}
