//! Finds the LLVM toolchain (clang, lld) that `lumen build --backend llvm`
//! shells out to. Mirrors toolchain.rs (gcc discovery): env override, PATH,
//! then a scan of known install roots.

use crate::toolchain::Tool;
use std::path::PathBuf;

#[cfg(windows)]
const EXE: &str = ".exe";
#[cfg(not(windows))]
const EXE: &str = "";

// Standard LLVM/MSYS2 install roots to scan, in priority order.
pub fn bins() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut push = |p: &str| v.push(PathBuf::from(p));

    // The Lumen-tuned LLVM fork (built from github.com/JamesKnight0001/lumen-llvm:
    // adds the gc "lumen" strategy + the lumen triple vendor). Preferred when
    // present, so Lumen-aware features are available.
    push("C:/llvm-build/bin");
    // MSYS2 mingw clang (x86_64-pc-windows-gnu, matches Lumen's gcc target).
    push("C:/msys64/mingw64/bin");
    push("C:/msys64/ucrt64/bin");
    push("C:/msys64/clang64/bin");
    // Official LLVM Windows installer.
    push("C:/Program Files/LLVM/bin");
    push("C:/Program Files (x86)/LLVM/bin");

    if let Ok(home) = std::env::var("USERPROFILE") {
        for rel in [
            // Lumen ships its own LLVM fork bundle here (installer drops it).
            "AppData/Local/Lumen/llvm/bin",
            "scoop/apps/llvm/current/bin",
            "scoop/shims",
        ] {
            v.push(PathBuf::from(format!("{home}/{rel}")));
        }
    }
    v
}

fn in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(format!("{name}{EXE}"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Locate an LLVM tool by name (e.g. "clang", "ld.lld"): env override, then
/// PATH, then a scan of the known install roots.
pub fn find(name: &str, env: Option<&str>) -> Option<Tool> {
    if let Some(var) = env {
        if let Ok(val) = std::env::var(var) {
            let p = PathBuf::from(val.trim());
            if p.is_file() {
                return Some(Tool {
                    bin_dir: p.parent().map(PathBuf::from),
                    program: p,
                    source: "override env",
                });
            }
        }
    }

    if let Some(p) = in_path(name) {
        return Some(Tool {
            bin_dir: p.parent().map(PathBuf::from),
            program: p,
            source: "PATH",
        });
    }

    for dir in bins() {
        let p = dir.join(format!("{name}{EXE}"));
        if p.is_file() {
            return Some(Tool {
                program: p,
                bin_dir: Some(dir),
                source: "auto-detected",
            });
        }
    }

    None
}

/// Locate clang, honoring `LUMEN_CLANG` then `CLANG`. clang drives the whole
/// chain: .ll -> .o (via its built-in llc) and the C runtime -> .o, then links.
pub fn find_clang() -> Option<Tool> {
    find("clang", Some("LUMEN_CLANG")).or_else(|| find("clang", Some("CLANG")))
}

/// Locate the LLVM linker. Prefer `ld.lld` (GNU-style, matches the gnu target);
/// fall back to `lld`. Honors `LUMEN_LLD`.
pub fn find_lld() -> Option<Tool> {
    find("ld.lld", Some("LUMEN_LLD")).or_else(|| find("lld", Some("LUMEN_LLD")))
}

/// Copy-pasteable hint shown when no LLVM toolchain is found.
pub fn install_hint() -> &'static str {
    if cfg!(windows) {
        "no LLVM toolchain found. `lumen build --backend llvm` links with clang + lld.\n\
         Install it, then Lumen picks it up automatically:\n\
         \x20 - MSYS2:  pacman -S mingw-w64-x86_64-clang mingw-w64-x86_64-lld\n\
         \x20 - winget: winget install -e --id LLVM.LLVM\n\
         \x20 - scoop:  scoop install llvm\n\
         Or point Lumen straight at clang:  set LUMEN_CLANG=C:\\path\\to\\clang.exe"
    } else {
        "no LLVM toolchain found. Install clang + lld (e.g. `apt install clang lld`),\n\
         or point Lumen at clang with  LUMEN_CLANG=/path/to/clang"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_nonempty() {
        assert!(!install_hint().is_empty());
    }

    #[test]
    fn missing_override() {
        std::env::remove_var("LUMEN_UNSET_CLANG");
        // Must not panic; result is host-dependent.
        let _ = find("clang", Some("LUMEN_UNSET_CLANG"));
    }
}
