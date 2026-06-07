//! Finds the GNU toolchain (gcc, windres) that `lumen build` shells out to.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Tool {
    pub program: PathBuf,
    pub bin_dir: Option<PathBuf>,
    /// How we found it, for `lumen doctor` / error messages.
    pub source: &'static str,
}

#[cfg(windows)]
const EXE: &str = ".exe";
#[cfg(not(windows))]
const EXE: &str = "";

fn candidate_bins() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut push = |p: &str| v.push(PathBuf::from(p));

    push("C:/msys64/mingw64/bin");
    push("C:/msys64/ucrt64/bin");
    push("C:/msys64/clang64/bin");
    push("C:/mingw64/bin");
    push("C:/MinGW/bin");
    push("C:/Program Files/mingw64/bin");
    push("C:/Program Files (x86)/mingw64/bin");

    if let Ok(home) = std::env::var("USERPROFILE") {
        for rel in [
            "scoop/apps/mingw/current/bin",
            "scoop/shims",
            ".rustup/toolchains/stable-x86_64-pc-windows-gnu/bin",
        ] {
            v.push(PathBuf::from(format!("{home}/{rel}")));
        }
    }
    v
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(format!("{name}{EXE}"));
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Locate a GNU tool by name (e.g. "gcc", "windres").
pub fn find_tool(name: &str, override_env: Option<&str>) -> Option<Tool> {
    if let Some(var) = override_env {
        if let Ok(val) = std::env::var(var) {
            let val = val.trim();
            if !val.is_empty() {
                let p = PathBuf::from(val);
                if p.is_file() {
                    let bin_dir = p.parent().map(PathBuf::from);
                    return Some(Tool {
                        program: p,
                        bin_dir,
                        source: "override env",
                    });
                }
            }
        }
    }

    if let Some(p) = on_path(name) {
        let bin_dir = p.parent().map(PathBuf::from);
        return Some(Tool {
            program: p,
            bin_dir,
            source: "PATH",
        });
    }

    for dir in candidate_bins() {
        let cand = dir.join(format!("{name}{EXE}"));
        if cand.is_file() {
            return Some(Tool {
                program: cand,
                bin_dir: Some(dir),
                source: "auto-detected",
            });
        }
    }

    None
}

/// Convenience: locate the C compiler, honoring `LUMEN_CC` then `CC`.
pub fn find_cc() -> Option<Tool> {
    if let Some(t) = find_tool("gcc", Some("LUMEN_CC")) {
        return Some(t);
    }
    find_tool("gcc", Some("CC"))
}

/// A human-readable, copy-pasteable hint for when no toolchain is found.
pub fn install_hint() -> &'static str {
    if cfg!(windows) {
        "no C toolchain found. Lumen links native binaries with gcc (MinGW-w64).\n\
         Install it, then `lumen build` will pick it up automatically:\n\
         \x20 - MSYS2:  https://www.msys2.org  then  pacman -S mingw-w64-ucrt-x86_64-gcc\n\
         \x20 - winget: winget install -e --id MSYS2.MSYS2\n\
         \x20 - scoop:  scoop install mingw\n\
         Or point Lumen straight at a compiler:  set LUMEN_CC=C:\\path\\to\\gcc.exe"
    } else {
        "no C toolchain found. Install gcc (e.g. `apt install gcc` / `xcode-select --install`),\n\
         or point Lumen at one with  LUMEN_CC=/path/to/gcc"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_env() {
        let me = PathBuf::from(file!());
        // file!() is relative to the crate root during tests.
        let abs = std::env::current_dir().unwrap().join(&me);
        if abs.is_file() {
            std::env::set_var("LUMEN_TEST_CC", abs.to_str().unwrap());
            let t = find_tool("gcc", Some("LUMEN_TEST_CC")).expect("override should resolve");
            assert_eq!(t.source, "override env");
            assert_eq!(t.program, abs);
            std::env::remove_var("LUMEN_TEST_CC");
        }
    }

    #[test]
    fn missing_override() {
        std::env::remove_var("LUMEN_DEFINITELY_UNSET_CC");
        // Should not panic; result depends on the host, we only assert it runs.
        let _ = find_tool("gcc", Some("LUMEN_DEFINITELY_UNSET_CC"));
    }

    #[test]
    fn install_hint() {
        assert!(!install_hint().is_empty());
    }
}
