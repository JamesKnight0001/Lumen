# Running and building a program

A Lumen file ends in `.lm`. One tool, `lumen`, runs it two ways and handles
everything around them.

## Run it now (the interpreter)

```
lumen run hello.lm
```

This walks your program directly: no build step, instant feedback. Use it while
writing code. It's also the *reference* implementation: if the interpreter and
compiler ever disagree, the interpreter is correct by definition.

## Build a real executable

```
lumen build hello.lm                 # produces hello.exe next to the source
lumen build hello.lm -o myapp.exe    # choose the output name
lumen build hello.lm --native        # tune for THIS machine's CPU (see below)
./myapp.exe
```

`lumen build` emits real x86-64 machine code, hands it to GCC to assemble and
link, and gives you a standalone native executable: no runtime, no bytecode, just
an `.exe`. This is what you ship and what the [benchmarks](performance.md)
measure.

### Tuning the optimizer (`--native`, `LUMEN_CC_OPT`)

By default the final GCC step runs `-O3 -flto`: portable, so the binary runs on
any x86-64 CPU. Two knobs trade that off:

- `--native` (or `LUMEN_MARCH=native`) adds `-march=native`, letting GCC use your
  exact CPU's instruction set. It measurably speeds up runtime-heavy code (maps,
  strings, GC), but the binary may not run on an older CPU, so it's opt-in. It
  does little for, and can even slightly slow, tight numeric loops, whose hot code
  is Lumen-emitted assembly `-march` doesn't touch.
- `LUMEN_CC_OPT` replaces the optimizer string outright: `LUMEN_CC_OPT="-O2"`, or
  `LUMEN_CC_OPT="-O0 -g"` for a fast debug build.

Neither knob changes what your program *prints*, only how fast it runs. The
interpreter/native byte-identical contract holds across all of them.

### Finding the C toolchain (you don't have to set PATH)

`lumen build` shells out to GCC (the MinGW-w64 toolchain on Windows) for the
final assemble-and-link step. You do **not** have to put it on your `PATH`.
Lumen finds it for you, in this order:

1. an explicit override (`LUMEN_CC`, then the generic `CC`) pointing at a
   `gcc.exe`;
2. `gcc` already on `PATH`;
3. a scan of the standard install roots: MSYS2 (`C:\msys64\{mingw64,ucrt64,
   clang64}\bin`), standalone MinGW-w64 (`C:\mingw64\bin`, ...), the Scoop
   mingw app, and the rustup `*-gnu` toolchain.

The first hit wins, and Lumen puts that toolchain's `bin` on the child process's
`PATH` so `gcc` finds its sibling `as`/`ld`. So a build that fails from a fresh
PowerShell (`could not run gcc`) works once GCC is installed anywhere standard:
no shell config needed.

To see exactly what Lumen finds, run `lumen doctor`:

```
lumen doctor
```

```
Lumen 0.77.0 - toolchain check

  gcc      found    C:\msys64\mingw64\bin\gcc.exe  (via auto-detected)
  windres  found    C:\msys64\mingw64\bin\windres.exe  (via auto-detected)  [optional, for --icon]

Native builds are ready: `lumen build file.lm -o file.exe`
```

If no compiler is found, `doctor` prints install instructions (MSYS2 / winget /
scoop) and exits non-zero. To force a compiler, set `LUMEN_CC`:

```
set LUMEN_CC=C:\path\to\gcc.exe      # cmd
$env:LUMEN_CC = 'C:\path\to\gcc.exe' # PowerShell
```

> **PowerShell `&&` note:** `lumen build app.lm -o app.exe && .\app.exe` fails in
> *Windows PowerShell 5.x* with `The token '&&' is not a valid statement
> separator`. That's PowerShell, not Lumen. Use `;` (runs the second command
> regardless), upgrade to PowerShell 7+ (which supports `&&`), or run the two
> commands on separate lines.

A few features (notably FFI callbacks) only work compiled: they need a real
machine-code address to hand the OS. The interpreter tells you clearly when you
hit one.

## The rest of the tool

```
lumen run    file.lm               run via the interpreter
lumen build  file.lm [-o out.exe]  compile to a native .exe
lumen -c     "<source>"            run a snippet inline (also: -e), like python -c
lumen repl                         start an interactive session
lumen new    <name>                scaffold a new project directory
lumen init                         scaffold a project in the current directory
lumen check  file.lm               parse + compile-check only, produce nothing
lumen emit   file.lm               print the generated x86-64 assembly
lumen install [pkg|url ...]        install packages into lumen_modules/ (none = from lumen.pkg)
lumen venv   <dir>                 create an isolated package environment
lumen update                       download + install a newer compiler (LUMEN_UPDATE_URL=owner/repo)
lumen doctor                       check the native-build toolchain (gcc, windres)
lumen tokens file.lm               dump the token stream (for debugging)
lumen ast    file.lm               dump the parsed syntax tree (for debugging)
lumen decls  file.lm               dump declaration name spans as TSV (tooling)
lumen version                      print the version
```

### `lumen decls` (tooling / language server)

`decls` parses one file and prints one tab-separated row per *named declaration*
(functions, methods, structs, fields, params, and imports), with each name's
exact source span:

```
kind<TAB>name<TAB>line<TAB>col<TAB>end_col<TAB>parent
```

Lines and columns are 1-based; `end_col` is exclusive; `parent` is the enclosing
struct (for fields/methods) or function (for params), empty otherwise. It runs
*before* import resolution, so it works on one file in isolation, what a language
server ([Lumenlance](https://github.com/lumen-lang/lumenlance)) needs for
go-to-definition and rename. The spans come from the parser, so they always match
how the compiler read your code.

This is a read-only front-end query: it shares the lexer and parser with `run`
and `build`, but collects spans into a side-table the normal compile path never
allocates, so it adds **zero** cost.

## The REPL

`lumen repl` gives an interactive prompt that remembers your definitions and
variables between lines. For a multi-line block, end a line with `:` and finish
with a blank line:

```
lumen> let x = 5
lumen> x * x
25
lumen> fn dbl(n):
  ...     return n * 2
  ...
lumen> dbl(21)
42
```

## Multi-file projects

You don't list every file. Point the compiler at the entry file and it follows
every `import` automatically:

```
lumen build myproject/main.lm -o app.exe
```

See [imports](../syntax/imports.md) for how modules resolve and combine.

## When things go wrong

Errors here are meant to be read, not decoded. Whether it's a parse error, a
compile-time type problem, or a runtime fault, Lumen prints a plain
`error: <message>` and points at the responsible line. Under `lumen run` you get
the offending source line with a caret underneath. A compiled `.exe` carries no
source text, so it points with `--> line N` instead. Either way, you get the
message and the location.
