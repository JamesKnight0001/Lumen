//! The `lumen` CLI. Parses subcommands (run, build, check, emit, repl, new,
//! tokens, ast, ...) and drives the pipeline: compile the source, then either
//! interpret it or hand the generated asm + bundled C runtime to gcc to make a
//! native .exe. Also handles project scaffolding and the --icon resource path.
use std::path::Path;
use std::process::Command;

use lumenc::{ast, codegen, interp, lexer, parser, CompileError};

// The C runtime is baked into the binary at compile time; `build` writes it out
// next to the generated asm so gcc can compile and link both together.
const LUMEN_RUNTIME: &str = include_str!("../runtime/lumen_rt.c");

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_usage();
        std::process::exit(2);
    }

    match args[1].as_str() {
        "repl" => return run_repl(),
        "version" | "--version" | "-V" => {
            println!("Lumen {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        "help" | "--help" | "-h" => return print_usage(),

        "doctor" => return run_doctor(),

        "new" => {
            let name = args.get(2).map(String::as_str).unwrap_or_else(|| {
                fatal("usage: lumen new <project-name>");
            });
            return scaffold(std::path::Path::new(name), name);
        }

        "init" => {
            let dir = std::env::current_dir().unwrap_or_else(|_| ".".into());
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "app".into());
            return scaffold(std::path::Path::new("."), &name);
        }

        "-c" | "-e" => {
            let code = args.get(2).map(String::as_str).unwrap_or_else(|| {
                fatal("usage: lumen -c \"<source>\"");
            });
            return run_source(code, std::path::Path::new("."));
        }

        // Package manager + venv + self-update. These have their own arg shapes
        // (or none), so handle them before the "needs a file" path below.
        "install" => {
            let pkgs: Vec<String> = args[2..].to_vec();
            return match lumenc::pkg::install(&pkgs) {
                Ok(()) => {}
                Err(e) => fatal(&format!("install error: {e}")),
            };
        }
        "venv" => {
            let dir = args.get(2).map(String::as_str).unwrap_or("venv");
            return match lumenc::pkg::venv(dir) {
                Ok(()) => {}
                Err(e) => fatal(&format!("venv error: {e}")),
            };
        }
        "update" => {
            return match lumenc::pkg::update() {
                Ok(()) => {}
                Err(e) => fatal(&format!("update error: {e}")),
            };
        }
        _ => {}
    }

    if args.len() < 3 {
        print_usage();
        std::process::exit(2);
    }

    let cmd = args[1].as_str();
    let file = &args[2];
    let src = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => fatal(&format!("cannot read {file}: {e}")),
    };

    if cmd == "tokens" {
        match lexer::Lexer::new(&src).tokenize() {
            Ok(toks) => toks.iter().for_each(|t| println!("{:?}", t.tok)),
            Err(e) => fatal(&format!("lex error: {e}")),
        }
        return;
    }

    // `decls`: emit declaration name spans as TSV for tooling (Lumenlance).
    // Format per line: kind<TAB>name<TAB>line<TAB>col<TAB>end_col<TAB>parent
    // Runs pre-compile so it works on a single file without resolving imports.
    if cmd == "decls" {
        match lumenc::parse_program_spanned(&src) {
            Ok((_, decls)) => {
                for d in &decls {
                    let kind = match d.kind {
                        ast::DeclKind::Fn => "fn",
                        ast::DeclKind::Method => "method",
                        ast::DeclKind::Struct => "struct",
                        ast::DeclKind::Field => "field",
                        ast::DeclKind::Param => "param",
                        ast::DeclKind::Import => "import",
                    };
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        kind,
                        d.name,
                        d.line,
                        d.col,
                        d.end_col,
                        d.parent.as_deref().unwrap_or("")
                    );
                }
            }
            Err(e) => fatal_compile(e, &src),
        }
        return;
    }

    let base_dir = entry_dir(file);
    // One compile front end for every command below; LUMEN_NO_OPT=1 disables the
    // optimizer (handy for debugging or comparing backends).
    let optimize = std::env::var("LUMEN_NO_OPT").as_deref() != Ok("1");
    let prog = match lumenc::compile(&src, &base_dir, optimize) {
        Ok(p) => p,
        Err(e) => fatal_compile(e, &src),
    };

    match cmd {
        "ast" => println!("{prog:#?}"),
        "check" => match codegen::Codegen::new().generate(&prog) {
            Ok(_) => println!("{file}: OK (parses and compiles)"),
            Err(e) => fatal_pretty("compile", &e, &src),
        },
        "run" => {
            let mut interp = interp::Interp::new();
            if let Err(e) = interp.run(&prog) {
                let line = interp.current_line();
                if line != 0 {
                    fatal_pretty("runtime", &format!("{e} (line {line})"), &src);
                }
                fatal_pretty("runtime", &e, &src);
            }
        }
        "emit" => match codegen::Codegen::new().generate(&prog) {
            Ok(asm) => println!("{asm}"),
            Err(e) => fatal_pretty("compile", &e, &src),
        },
        "build" => build(&prog, file, &src, &args),
        other => {
            eprintln!("unknown command: {other}");
            std::process::exit(2);
        }
    }
}

// Explicit backend pick, if any: --backend <x>, --backend=<x>, or LUMEN_BACKEND.
// Returns lowercased "llvm"/"asm"/"gcc"; None means "use the default".
fn backend_choice(args: &[String]) -> Option<String> {
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix("--backend=") {
            return Some(v.to_lowercase());
        }
        if a == "--backend" {
            if let Some(v) = args.get(i + 1) {
                return Some(v.to_lowercase());
            }
        }
    }
    std::env::var("LUMEN_BACKEND").ok().map(|v| v.to_lowercase())
}

fn build(prog: &ast::Program, file: &str, src: &str, args: &[String]) {
    // Backend selector. LLVM is the DEFAULT (emits IR, links via clang+lld); if
    // the LLVM toolchain isn't installed we fall back to the asm+gcc backend.
    // Override either way: LUMEN_BACKEND=llvm|asm (or gcc), or --backend <x>.
    let pick = backend_choice(args);
    let use_llvm = match pick.as_deref() {
        Some("llvm") => true,
        Some("asm") | Some("gcc") => false,
        // default: LLVM when clang+lld are present, else fall back to gcc
        _ => {
            let ok = lumenc::llvm::find_clang().is_some() && lumenc::llvm::find_lld().is_some();
            if !ok {
                eprintln!("note: LLVM toolchain not found, using the gcc backend");
            }
            ok
        }
    };
    if use_llvm {
        return build_llvm(prog, file, src, args);
    }

    let asm = match codegen::Codegen::new().generate(prog) {
        Ok(a) => a,
        Err(e) => fatal_pretty("compile", &e, src),
    };

    let mut out = format!(
        "{}.exe",
        Path::new(file).file_stem().unwrap().to_string_lossy()
    );
    let mut icon: Option<String> = None;
    let mut native = false; // --native: add -march=native (non-portable, this box)
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "-o" if i + 1 < args.len() => {
                out = args[i + 1].clone();
                i += 2;
            }
            "--icon" if i + 1 < args.len() => {
                icon = Some(args[i + 1].clone());
                i += 2;
            }
            "--native" => {
                native = true;
                i += 1;
            }
            "--backend" => {
                i += 2; // consume "--backend <x>" (already handled by backend_choice)
            }
            a if a.starts_with("--backend=") => {
                i += 1;
            }
            other => {
                eprintln!("warning: ignoring unknown build flag '{other}'");
                i += 1;
            }
        }
    }

    let stem = out.trim_end_matches(".exe");
    let asm_path = format!("{stem}.s");
    if let Err(e) = std::fs::write(&asm_path, &asm) {
        fatal(&format!("error writing asm: {e}"));
    }
    let rt_path = format!("{stem}_rt.c");
    if let Err(e) = std::fs::write(&rt_path, LUMEN_RUNTIME) {
        fatal(&format!("error writing runtime: {e}"));
    }

    let icon_obj = icon.as_deref().and_then(|p| make_icon(p, stem));

    // Find gcc ourselves (env override / PATH / known roots) so builds work from
    // a fresh shell with no MinGW on PATH. -ffunction-sections + --gc-sections
    // drop unused code; extern blocks add their own -l flags below.
    let cc = lumenc::toolchain::find_cc()
        .unwrap_or_else(|| fatal(&format!("error: {}", lumenc::toolchain::install_hint())));
    let mut cmd = Command::new(&cc.program);
    // Put gcc's own bin on the child PATH so it finds its as/ld.
    prepend_path(&mut cmd, cc.bin_dir.as_deref());
    // Opt level: -O3 -flto by default; LUMEN_CC_OPT overrides (e.g. "-O2" or
    // "-O0 -g"). --native or LUMEN_MARCH=native adds -march=native, which bakes
    // in this box's ISA (faster, non-portable), so it stays opt-in.
    let opt = std::env::var("LUMEN_CC_OPT").unwrap_or_else(|_| "-O3 -flto".into());
    let want_native = native || std::env::var("LUMEN_MARCH").as_deref() == Ok("native");
    for f in opt.split_whitespace() {
        cmd.arg(f);
    }
    if want_native {
        cmd.arg("-march=native");
    }
    cmd.args([
        "-s",
        "-ffunction-sections",
        "-fdata-sections",
        "-Wl,--gc-sections",
        "-o",
        &out,
        &asm_path,
        &rt_path,
        "-lm",
        "-lws2_32", // net module (Winsock2); --gc-sections drops it if unused
    ]);
    if let Some(obj) = &icon_obj {
        cmd.arg(obj);
    }
    for lib in collect_extlibs(prog) {
        cmd.arg(format!("-l{lib}"));
    }
    match cmd.status() {
        Ok(s) if s.success() => {
            let _ = std::fs::remove_file(&rt_path);
            if let Some(obj) = &icon_obj {
                let _ = std::fs::remove_file(obj);
            }
            println!("built {out}  (assembly: {asm_path})");
        }
        Ok(s) => fatal(&format!("linker failed with status {:?}", s.code())),
        Err(e) => fatal(&format!(
            "could not run gcc (is the GNU toolchain installed?): {e}"
        )),
    }
}

// Prepend `dir` to the child command's PATH so a located tool finds its
// siblings (gcc -> as/ld) even when the parent shell's PATH lacks them.
fn prepend_path(cmd: &mut Command, dir: Option<&Path>) {
    let Some(dir) = dir else { return };
    let mut paths = vec![dir.to_path_buf()];
    if let Some(old) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&old));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        cmd.env("PATH", joined);
    }
}

fn build_llvm(prog: &ast::Program, _file: &str, src: &str, args: &[String]) {
    let ll = match lumenc::llvmgen::LlvmGen::new().generate(prog) {
        Ok(s) => s,
        Err(e) => fatal_pretty("compile", &e, src),
    };

    // parse -o out / default name from args (reuse the same shape as build)
    let mut out = String::from("a.exe");
    let mut i = 3;
    while i < args.len() {
        if args[i] == "-o" && i + 1 < args.len() {
            out = args[i + 1].clone();
            i += 2;
        } else {
            i += 1;
        }
    }
    if out == "a.exe" {
        out = format!(
            "{}.exe",
            Path::new(_file).file_stem().unwrap().to_string_lossy()
        );
    }

    let stem = out.trim_end_matches(".exe");
    let ll_path = format!("{stem}.ll");
    if let Err(e) = std::fs::write(&ll_path, &ll) {
        fatal(&format!("error writing ir: {e}"));
    }
    let rt_path = format!("{stem}_rt.c");
    if let Err(e) = std::fs::write(&rt_path, LUMEN_RUNTIME) {
        fatal(&format!("error writing runtime: {e}"));
    }

    // clang drives the whole chain: .ll -> .o, the C runtime -> .o, link via lld.
    let clang = lumenc::llvm::find_clang()
        .unwrap_or_else(|| fatal(&format!("error: {}", lumenc::llvm::install_hint())));
    // -O3, no LTO. LTO miscompiles the try/catch path (it reorders work across
    // the runtime's custom setjmp/longjmp), so we keep it off for correctness.
    // -O3 + the unboxed-int fast path is byte-identical at every opt level and
    // captures the bulk of the speedup. Override with LUMEN_LLVM_OPT if desired.
    let opt = std::env::var("LUMEN_LLVM_OPT").unwrap_or_else(|_| "-O3".into());
    let mut cmd = Command::new(&clang.program);
    prepend_path(&mut cmd, clang.bin_dir.as_deref());
    for f in opt.split_whitespace() {
        cmd.arg(f);
    }
    cmd.args([
        "-fuse-ld=lld",
        "-Wno-override-module",
        "-o",
        &out,
        &ll_path,
        &rt_path,
        "-lm",
        "-lws2_32",
    ]);
    for lib in collect_extlibs(prog) {
        cmd.arg(format!("-l{lib}"));
    }
    match cmd.status() {
        Ok(s) if s.success() => {
            let _ = std::fs::remove_file(&rt_path);
            println!("built {out}  (llvm ir: {ll_path})");
        }
        Ok(s) => fatal(&format!("clang/lld failed with status {:?}", s.code())),
        Err(e) => fatal(&format!(
            "could not run clang (is the LLVM toolchain installed?): {e}"
        )),
    }
}

fn make_icon(icon_path: &str, stem: &str) -> Option<String> {
    let p = Path::new(icon_path);
    if !p.exists() {
        eprintln!("warning: --icon '{icon_path}' not found; building without an icon");
        return None;
    }

    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let ico_path: String = if ext == "ico" {
        icon_path.to_string()
    } else if ext == "png" {
        match png2ico(icon_path, stem) {
            Some(path) => path,
            None => {
                eprintln!("warning: could not read PNG '{icon_path}'; building without an icon");
                return None;
            }
        }
    } else {
        eprintln!("warning: --icon expects a .ico or .png (got '{icon_path}'); skipping");
        return None;
    };

    let abs = std::fs::canonicalize(&ico_path).unwrap_or_else(|_| ico_path.clone().into());
    let ico_str = abs
        .display()
        .to_string()
        .replace('\\', "/")
        .replace("//?/", "");
    let rc_path = format!("{stem}_icon.rc");
    let obj_path = format!("{stem}_icon.o");
    if std::fs::write(&rc_path, format!("1 ICON \"{ico_str}\"\n")).is_err() {
        eprintln!("warning: could not write resource script; building without an icon");
        return None;
    }
    // windres ships with gcc; find it the same way.
    let Some(windres) = lumenc::toolchain::find_tool("windres", Some("LUMEN_WINDRES")) else {
        eprintln!("warning: windres not found; building without an icon");
        let _ = std::fs::remove_file(&rc_path);
        return None;
    };
    let mut wcmd = Command::new(&windres.program);
    prepend_path(&mut wcmd, windres.bin_dir.as_deref());
    let status = wcmd
        .args([
            "--input",
            &rc_path,
            "--output",
            &obj_path,
            "--output-format=coff",
        ])
        .status();
    let _ = std::fs::remove_file(&rc_path);

    if ext == "png" {
        let _ = std::fs::remove_file(&ico_path);
    }
    match status {
        Ok(s) if s.success() => Some(obj_path),
        Ok(_) => {
            eprintln!("warning: windres failed; building without an icon (is it on PATH?)");
            None
        }
        Err(e) => {
            eprintln!("warning: could not run windres ({e}); building without an icon");
            None
        }
    }
}

// Wrap a PNG in a minimal single-image .ico container so windres can embed it.
// Reads width/height straight from the PNG IHDR (bytes 16..24), then writes the
// 6-byte ICONDIR + 16-byte ICONDIRENTRY header followed by the raw PNG bytes.
// Width/height >= 256 are stored as 0 per the ICO spec.
fn png2ico(png_path: &str, stem: &str) -> Option<String> {
    let png = std::fs::read(png_path).ok()?;

    if png.len() < 24 || &png[0..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);

    let wb = if w >= 256 { 0u8 } else { w as u8 };
    let hb = if h >= 256 { 0u8 } else { h as u8 };

    let mut ico = Vec::with_capacity(22 + png.len());

    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]);

    ico.push(wb);
    ico.push(hb);
    ico.push(0);
    ico.push(0);
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&32u16.to_le_bytes());
    ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(&png);

    let path = format!("{stem}_icontmp.ico");
    std::fs::write(&path, &ico).ok()?;
    Some(path)
}

fn collect_extlibs(prog: &ast::Program) -> Vec<String> {
    let mut libs = Vec::new();
    for item in prog {
        if let ast::Item::ExternBlock(b) = item {
            let base = b
                .lib
                .trim_end_matches(".dll")
                .trim_end_matches(".so")
                .to_string();
            if !base.is_empty() && !libs.contains(&base) {
                libs.push(base);
            }
        }
    }
    libs
}

fn entry_dir(file: &str) -> std::path::PathBuf {
    Path::new(file)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn fatal(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

fn fatal_pretty(kind: &str, msg: &str, src: &str) -> ! {
    let (headline, line_no) = match extract_line_no(msg) {
        Some(n) => {
            let trimmed = msg
                .rfind(" (line ")
                .map(|i| &msg[..i])
                .unwrap_or(msg)
                .trim_end();
            (trimmed.to_string(), Some(n))
        }
        None => (msg.to_string(), None),
    };
    let tag = if kind.is_empty() {
        "error".to_string()
    } else {
        format!("{kind} error")
    };
    eprintln!("{tag}: {headline}");
    if let Some(n) = line_no {
        if let Some(text) = src.lines().nth(n - 1) {
            eprintln!("  {n:>4} | {text}");
            let indent = text.len().saturating_sub(text.trim_start().len());
            eprintln!("       | {}^", " ".repeat(indent));
        } else {
            eprintln!("  (at line {n})");
        }
    }
    std::process::exit(1);
}

fn fatal_compile(err: CompileError, src: &str) -> ! {
    let (kind, msg) = match &err {
        CompileError::Lex(m) => ("lex", m.clone()),
        CompileError::Parse(m) => ("parse", m.clone()),
        CompileError::Import(m) => ("import", m.clone()),
    };
    fatal_pretty(kind, &msg, src);
}

fn extract_line_no(msg: &str) -> Option<usize> {
    let idx = msg.find("line ")? + 5;
    let digits: String = msg[idx..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn run_source(src: &str, base_dir: &std::path::Path) {
    let optimize = std::env::var("LUMEN_NO_OPT").as_deref() != Ok("1");
    let prog = match lumenc::compile(src, base_dir, optimize) {
        Ok(p) => p,
        Err(e) => fatal_compile(e, src),
    };
    let mut interp = interp::Interp::new();
    if let Err(e) = interp.run(&prog) {
        let line = interp.current_line();
        if line != 0 {
            fatal_pretty("runtime", &format!("{e} (line {line})"), src);
        }
        fatal_pretty("runtime", &e, src);
    }
}

fn scaffold(dir: &std::path::Path, name: &str) {
    if dir != std::path::Path::new(".") {
        if dir.exists() {
            fatal(&format!("error: '{}' already exists", dir.display()));
        }
        if let Err(e) = std::fs::create_dir_all(dir) {
            fatal(&format!("error: cannot create '{}': {e}", dir.display()));
        }
    }
    let main_lm = format!(
        "#[\n {name} - a Lumen project.\n\nlumen run main.lm <- run via the interpreter\nlumen build main.lm -o {name}.exe && ./{name}.exe <- native binary\n]#\n\nfn greet(who):\n    return f\"Hello, {{who}}!\"\n\nfn main():\n    print(greet(\"{name}\"))\n"
    );
    let gitignore = "*.exe\n*.s\n*_rt.c\n";
    let readme = format!(
        "# {name}\n\nA Lumen project. Run it:\n\n```sh\nlumen run main.lm\n```\n\nBuild a native executable:\n\n```sh\nlumen build main.lm -o {name}.exe\n./{name}.exe\n```\n"
    );
    let write = |rel: &str, contents: &str| {
        let p = dir.join(rel);
        if let Err(e) = std::fs::write(&p, contents) {
            fatal(&format!("error: cannot write {}: {e}", p.display()));
        }
    };
    write("main.lm", &main_lm);
    write(".gitignore", gitignore);
    write("README.md", &readme);
    if dir == std::path::Path::new(".") {
        println!("Initialized Lumen project '{name}' in the current directory.");
    } else {
        println!("Created Lumen project '{name}' in {}/", dir.display());
    }
    println!(
        "Next:  cd {} && lumen run main.lm",
        if dir == std::path::Path::new(".") {
            "."
        } else {
            name
        }
    );
}

fn print_usage() {
    eprintln!(
        "Lumen {} - the Lumen programming language\n",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!("USAGE:");
    eprintln!("  lumen run    <file.lm>               run via the Tier-0 interpreter");
    eprintln!(
        "  lumen build  <file.lm> [-o out.exe] [--icon <p>] [--native] [--backend llvm|asm]  compile to a native .exe (LLVM by default, gcc fallback)"
    );
    eprintln!("  lumen -c     \"<source>\"               run inline source");
    eprintln!("  lumen new    <name>                  scaffold a new project directory");
    eprintln!("  lumen init                           scaffold a project in the cwd");
    eprintln!("  lumen check  <file.lm>               parse + compile-check, no output");
    eprintln!("  lumen emit   <file.lm>               print generated x86-64 assembly");
    eprintln!("  lumen install [pkg|url ...]          install packages into lumen_modules/ (none = from lumen.pkg)");
    eprintln!("  lumen venv   <dir>                   create an isolated package environment");
    eprintln!("  lumen update                         download + install a newer compiler (LUMEN_UPDATE_URL=owner/repo)");
    eprintln!("  lumen repl                           interactive read-eval-print loop");
    eprintln!(
        "  lumen doctor                         check the native-build toolchain (gcc, windres)"
    );
    eprintln!("  lumen tokens <file.lm>               dump tokens (debug)");
    eprintln!("  lumen ast    <file.lm>               dump the AST (debug)");
    eprintln!(
        "  lumen decls  <file.lm>               dump declaration name spans as TSV (tooling)"
    );
    eprintln!("  lumen version                        print the version");
}

// `lumen doctor`: report whether the native-build toolchain is reachable
fn run_doctor() {
    println!("Lumen {} - toolchain check\n", env!("CARGO_PKG_VERSION"));
    let mut ok = true;
    match lumenc::toolchain::find_cc() {
        Some(t) => println!(
            "  gcc      found    {}  (via {})",
            t.program.display(),
            t.source
        ),
        None => {
            ok = false;
            println!("  gcc      MISSING  (required for `lumen build`)");
        }
    }
    match lumenc::toolchain::find_tool("windres", Some("LUMEN_WINDRES")) {
        Some(t) => println!(
            "  windres  found    {}  (via {})  [optional, for --icon]",
            t.program.display(),
            t.source
        ),
        None => println!("  windres  missing  (optional; only needed for --icon)"),
    }
    println!();
    if ok {
        println!("Native builds are ready: `lumen build file.lm -o file.exe`");
    } else {
        println!("{}", lumenc::toolchain::install_hint());
        std::process::exit(1);
    }
}

fn run_repl() {
    use std::collections::HashMap;
    use std::io::{self, Write};

    println!(
        "Lumen {} REPL - type :help for commands, :quit to exit",
        env!("CARGO_PKG_VERSION")
    );
    let mut interp = interp::Interp::new();
    let mut env: HashMap<String, interp::Value> = HashMap::new();
    let stdin = io::stdin();

    loop {
        print!("lumen> ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            println!();
            break;
        }
        let trimmed = line.trim_end();
        match trimmed.trim() {
            "" => continue,
            ":quit" | ":q" | "exit" => break,
            ":help" | ":h" => {
                println!(":quit  exit   |   :help  this message");
                println!("Enter any Lumen statement or expression. Multi-line blocks:");
                println!(
                    "end a header with ':' then keep typing indented lines, blank line to run."
                );
                continue;
            }
            _ => {}
        }

        let mut buf = line.clone();
        // A line ending in `:` opens a block, so keep reading indented
        // continuation lines until a blank line, then evaluate the whole thing.
        if trimmed.ends_with(':') {
            loop {
                print!("  ... ");
                let _ = io::stdout().flush();
                let mut cont = String::new();
                if stdin.read_line(&mut cont).unwrap_or(0) == 0 || cont.trim().is_empty() {
                    break;
                }
                buf.push_str(&cont);
            }
        }

        let toks = match lexer::Lexer::new(&buf).tokenize() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("lex error: {e}");
                continue;
            }
        };
        let prog = match parser::Parser::new(toks).parse_program() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("parse error: {e}");
                continue;
            }
        };
        let stmts = interp.register_decls(&prog);
        for s in &stmts {
            match interp.repl_exec(s, &mut env) {
                Ok(Some(v)) if !matches!(v, interp::Value::Nil) => println!("{v}"),
                Ok(_) => {}
                Err(e) => eprintln!("runtime error: {e}"),
            }
        }
    }
    println!("bye.");
}
