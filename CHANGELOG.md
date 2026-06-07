# Changelog

All notable changes to the Lumen compiler are recorded here.

## 0.74.0

### Added
- **`net` module** - Winsock2-backed TCP and UDP sockets. TCP:
  `net.listen/accept/connect`; UDP: `net.udp/sendto/recvfrom`; shared
  `net.send/recv/close/shutdown`. Advanced controls for real network code:
  `net.set_timeout`, `net.set_blocking`, `net.set_opt` (reuseaddr, keepalive,
  broadcast, sndbuf, rcvbuf, nodelay), `net.poll` (select-based readiness),
  `net.resolve` (DNS), `net.local_port`, and `net.errno`. A socket is an int
  handle (`-1` = error); `recvfrom` returns `{data, host, port}`. Implemented on
  both backends - the interpreter binds Winsock via FFI, the native runtime
  calls it directly - so output is byte-identical. Windows-only. See
  `examples/19_net.lm` and [stdlib.md](docs/syntax/stdlib.md).
- **Package manager** - `lumen install <pkg|url ...>` downloads packages (single
  `.lm` modules) into a `lumen_modules/` directory that `import` now searches
  after sibling files and builtins. Bare names resolve against a registry
  (`LUMEN_REGISTRY`, default GitHub). A `lumen.pkg` manifest records deps for a
  reproducible bare `lumen install`; packages declare transitive deps with a
  `#!dep <name> <source>` line (de-duplicated, cycle-safe).
- **Virtual environments** - `lumen venv <dir>` creates an isolated
  `lumen_modules/`; set `LUMEN_VENV` to install into and resolve from it.
- **Self-update** - `lumen update` downloads a newer compiler and swaps it in,
  keeping a `.old.exe` backup. `LUMEN_UPDATE_URL` accepts a GitHub release repo
  (`owner/repo` or `https://github.com/owner/repo`, resolved to the latest
  release's `lumen.exe` asset) or a direct `.exe` URL.

### Fixed
- **`str()` of a list / map / struct now formats its contents** on the native
  backend instead of printing `<obj>`. It already worked under the interpreter
  and via `print(...)`; only `str(compound)` on a compiled binary was wrong.
  Both backends now produce identical text (a whole-class fix routing
  `lumen_to_str` through the same formatter as `print`). New `str()`-of-compound
  coverage; existing examples only ever printed compounds, which is why the
  differential suite had not caught it.

### Notes
- `net`, `lumen install`, and `lumen update` are Windows-only. `net` uses
  Winsock2; the native build links `-lws2_32` (dropped by `--gc-sections` when a
  program imports no `net`). Package downloads use an internal WinHTTP client.

## 0.73.0

### Fixed
- **Parser now rejects trailing junk after a statement.** Previously a leaf
  statement followed by stray tokens on the same line (e.g. `return x  999`,
  `let a = 1 2 3`, a bare `42` followed by more, or `f() garbage`) was silently
  accepted - the parser dropped the extra tokens and started a new statement.
  It now requires every leaf statement to end at a line boundary (Newline - also
  produced by `;` - Dedent, or Eof) and reports
  `unexpected <tok> after statement (expected end of line)`. Compound statements
  (`if`/`while`/`for`/`try`) remain self-terminating; `;` as a statement
  separator is unaffected. Parse-time only: emitted assembly is byte-identical.

## 0.72.0

### Added
- **`lumen decls <file.lm>`** - a tooling/language-server command that prints
  every named declaration (functions, methods, structs, fields, params,
  imports, and `extern` FFI fns) with its exact source span, as TSV:
  `kind<TAB>name<TAB>line<TAB>col<TAB>end_col<TAB>parent`. Runs before import
  resolution so it works on a single file in isolation.
- **`lumenc::parse_program_spanned`** - a public library entry that parses and
  returns the AST plus a `Vec<DeclSpan>` side-table of declaration name spans.
  New public types `ast::DeclSpan` and `ast::DeclKind`.

### Performance
- **Zero impact on `run`/`build`.** Span collection is a parser side-table built
  only by the spanned entry point; the normal compile path allocates nothing
  extra and runs byte-for-byte identical code. Verified: emitted x86-64 assembly
  is unchanged (sha256 identical for `bench/fib.lm` and `bench/intloop.lm`) and
  native runtime is within noise of the 0.71.0 baseline.

### Notes
- These additions back the [Lumenlance](../Lumenlance) language server's
  navigation (go-to-definition, rename, outline). The server cross-checks its
  resilient token recognizer against these compiler-authoritative spans over the
  whole example corpus (34/34 files agree).
