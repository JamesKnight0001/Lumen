//! Package manager + venv + self-update CLI (the `lumen install/venv/update`
//! subcommands). Packages are single .lm files fetched over HTTP into a
//! `lumen_modules/` dir that the import resolver searches (see imports.rs).
//! Pure tooling: no codegen, interpreter-independent. Windows-only (uses the
//! WinHTTP-backed http::fetch).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DEF_REGISTRY: &str = "https://raw.githubusercontent.com/lumen-lang/packages/main";
const MANIFEST: &str = "lumen.pkg";
const VENV_MARK: &str = "lumen-venv.cfg";

// Where packages install / are resolved from. Active venv (LUMEN_VENV) wins,
// else a project-local lumen_modules/ in the cwd.
pub fn modules_dir() -> PathBuf {
    if let Ok(v) = std::env::var("LUMEN_VENV") {
        if !v.is_empty() {
            return Path::new(&v).join("lumen_modules");
        }
    }
    PathBuf::from("lumen_modules")
}

// Registry base for bare-name installs; override with LUMEN_REGISTRY.
fn registry() -> String {
    std::env::var("LUMEN_REGISTRY").unwrap_or_else(|_| DEF_REGISTRY.to_string())
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

// A dep "source" is either a URL or a bare name resolved against the registry
// as <registry>/<name>/<name>.lm. Returns (name, url).
fn resolve(src: &str) -> (String, String) {
    if is_url(src) {
        let base = src.rsplit('/').next().unwrap_or(src);
        let name = base.strip_suffix(".lm").unwrap_or(base).to_string();
        (name, src.to_string())
    } else {
        let url = format!("{}/{}/{}.lm", registry(), src, src);
        (src.to_string(), url)
    }
}

// `lumen install [pkg-or-url ...]`. No args = install everything in lumen.pkg.
pub fn install(args: &[String]) -> Result<(), String> {
    let root = modules_dir();
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;

    let specs: Vec<(String, String)> = if args.is_empty() {
        let deps = read_manifest();
        if deps.is_empty() {
            return Err(format!(
                "nothing to install: pass a package/URL, or add deps to {MANIFEST}"
            ));
        }
        deps.iter().map(|(_, s)| resolve(s)).collect()
    } else {
        args.iter().map(|s| resolve(s)).collect()
    };

    let mut seen = HashSet::new();
    for (name, url) in &specs {
        install_one(name, url, &root, &mut seen)?;
        // Record top-level deps so `lumen install` (no args) is reproducible.
        if args.is_empty() {
            // installing FROM the manifest; don't rewrite it
        } else {
            add_dep(name, url);
        }
    }
    println!("done ({} package(s) in {})", seen.len(), root.display());
    Ok(())
}

// Fetch one package + recurse into its embedded deps. Dedup by name keeps a
// diamond/cycle of deps terminating (mirrors imports::collect).
fn install_one(
    name: &str,
    url: &str,
    root: &Path,
    seen: &mut HashSet<String>,
) -> Result<(), String> {
    if !seen.insert(name.to_string()) {
        return Ok(()); // already handled this run
    }
    println!("installing {name}  <- {url}");
    let bytes = crate::http::fetch(url)?;
    let src = String::from_utf8_lossy(&bytes).into_owned();
    let dest = root.join(format!("{name}.lm"));
    std::fs::write(&dest, &src).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;

    for (dn, ds) in scan_deps(&src) {
        let (rn, ru) = resolve(&ds);
        let _ = dn; // the directive name is informational; resolve owns the real name
        install_one(&rn, &ru, root, seen)?;
    }
    println!("  ok {name} ({} bytes)", bytes.len());
    Ok(())
}

// Embedded dependency directives: a line `#!dep <name> <source>` near the top
// of a package declares a transitive dep. Cheap, needs no sidecar file.
fn scan_deps(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("#!dep ") {
            let mut it = rest.split_whitespace();
            if let Some(n) = it.next() {
                let s = it.next().unwrap_or(n);
                out.push((n.to_string(), s.to_string()));
            }
        }
    }
    out
}

// ---- manifest (lumen.pkg): line-based, [deps] section of `name = source` ----
fn read_manifest() -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(MANIFEST) else {
        return Vec::new();
    };
    let mut deps = Vec::new();
    let mut in_deps = false;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') {
            in_deps = t == "[deps]";
            continue;
        }
        if in_deps {
            if let Some((k, v)) = t.split_once('=') {
                deps.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
    }
    deps
}

// Add/update one dep line in the manifest's [deps] section, creating the file
// (and the section) if absent. Idempotent on name.
fn add_dep(name: &str, source: &str) {
    let mut text = std::fs::read_to_string(MANIFEST).unwrap_or_default();
    if text.is_empty() {
        text = format!("# Lumen package manifest\nname = app\nversion = 0.1.0\n\n[deps]\n");
    }
    if !text.contains("[deps]") {
        text.push_str("\n[deps]\n");
    }
    let line = format!("{name} = {source}");
    // Replace an existing entry for this name, else append under [deps].
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let mut replaced = false;
    for l in lines.iter_mut() {
        if l.trim_start().starts_with(&format!("{name} "))
            || l.trim_start().starts_with(&format!("{name}="))
        {
            *l = line.clone();
            replaced = true;
            break;
        }
    }
    if !replaced {
        let pos = lines.iter().position(|l| l.trim() == "[deps]").unwrap();
        lines.insert(pos + 1, line);
    }
    let _ = std::fs::write(MANIFEST, lines.join("\n") + "\n");
}

// ---- venv ----

// `lumen venv <dir>`: make an isolated package dir + marker. Print how to
// activate it (set LUMEN_VENV) so installs/imports use it.
pub fn venv(dir: &str) -> Result<(), String> {
    let base = Path::new(dir);
    let mods = base.join("lumen_modules");
    std::fs::create_dir_all(&mods).map_err(|e| format!("cannot create {}: {e}", mods.display()))?;
    let mark = base.join(VENV_MARK);
    std::fs::write(&mark, format!("version = {}\n", env!("CARGO_PKG_VERSION")))
        .map_err(|e| format!("cannot write {}: {e}", mark.display()))?;
    let abs = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let shown = abs.display().to_string().replace("\\\\?\\", "");
    println!("created Lumen venv at {shown}");
    println!("activate it for this shell:");
    println!("  export LUMEN_VENV=\"{shown}\"        # bash");
    println!("  set LUMEN_VENV={shown}               # cmd");
    println!("then `lumen install <pkg>` installs into it and `lumen run` resolves from it.");
    Ok(())
}

// ---- self update ----
// Resolve LUMEN_UPDATE_URL into a direct download URL for lumen.exe. Accepts a
// GitHub repo as `owner/repo` or `https://github.com/owner/repo` (grabs the
// latest release's lumen.exe asset), or a direct .exe URL (used verbatim).
// GitHub's releases/latest/download path 302-redirects to the asset; WinHTTP
// follows the HTTPS->HTTPS redirect for us, so no API/JSON is needed.
fn update_url(spec: &str) -> Result<String, String> {
    let s = spec.trim().trim_end_matches('/');
    if s.ends_with(".exe") {
        return Ok(s.to_string()); // direct binary URL
    }
    let repo = s
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("github.com/")
        .trim_end_matches(".git");
    let parts: Vec<&str> = repo.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() != 2 {
        return Err(format!(
            "LUMEN_UPDATE_URL must be a GitHub repo (owner/repo or \
             https://github.com/owner/repo) or a direct .exe URL; got: {spec}"
        ));
    }
    Ok(format!(
        "https://github.com/{}/{}/releases/latest/download/lumen.exe",
        parts[0], parts[1]
    ))
}

// `lumen update`: download a fresh compiler and swap it in. On Windows a running
// .exe can be renamed but not overwritten, so move self aside then drop the new
// binary in place. URL is configurable; no silent magic.
pub fn update() -> Result<(), String> {
    let spec = std::env::var("LUMEN_UPDATE_URL").unwrap_or_default();
    if spec.trim().is_empty() {
        return Err(
            "no update source: set LUMEN_UPDATE_URL to a GitHub release repo or a\n  \
             direct lumen.exe URL, e.g.\n  \
             export LUMEN_UPDATE_URL=JamesKnight0001/Lumen\n  \
             export LUMEN_UPDATE_URL=https://example.com/lumen.exe"
                .into(),
        );
    }
    let url = update_url(&spec)?;
    println!("downloading compiler  <- {url}");
    let bytes = crate::http::fetch(&url)?;
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate current exe: {e}"))?;
    let old = exe.with_extension("old.exe");
    let _ = std::fs::remove_file(&old); // clear a stale backup
    std::fs::rename(&exe, &old).map_err(|e| format!("cannot move current exe aside: {e}"))?;
    if let Err(e) = std::fs::write(&exe, &bytes) {
        let _ = std::fs::rename(&old, &exe); // roll back
        return Err(format!("cannot write new exe: {e}"));
    }
    println!(
        "updated {} ({} bytes). previous binary kept at {}",
        exe.display(),
        bytes.len(),
        old.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::update_url;

    const LATEST: &str = "https://github.com/o/r/releases/latest/download/lumen.exe";

    #[test]
    fn resolve_github() {
        for spec in [
            "o/r",
            "o/r/",
            "https://github.com/o/r",
            "https://github.com/o/r/",
            "http://github.com/o/r",
            "github.com/o/r",
            "https://github.com/o/r.git",
            "  o/r  ",
        ] {
            assert_eq!(update_url(spec).unwrap(), LATEST, "spec: {spec}");
        }
    }

    #[test]
    fn resolve_direct() {
        let u = "https://example.com/builds/lumen.exe";
        assert_eq!(update_url(u).unwrap(), u);
    }

    #[test]
    fn resolve_bad() {
        for spec in ["", "   ", "owner", "a/b/c", "https://github.com/owner"] {
            assert!(update_url(spec).is_err(), "should reject: {spec:?}");
        }
    }
}
