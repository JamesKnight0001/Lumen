# Changelog

All notable changes to the Lumen compiler are recorded here.

## 0.73.0

### Fixed
- **Parser now rejects trailing junk after a statement.** Previously a leaf
  statement followed by stray tokens on the same line (e.g. `return x  999`,
  `let a = 1 2 3`, a bare `42` followed by more, or `f() garbage`) was silently
  accepted — the parser dropped the extra tokens and started a new statement.
  It now requires every leaf statement to end at a line boundary (Newline — also
  produced by `;` — Dedent, or Eof) and reports
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
