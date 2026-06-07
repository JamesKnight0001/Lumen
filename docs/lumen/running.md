# Running and building a program

A Lumen file ends in `.lm`. You can run it two ways, and one command-line tool
(`lumen`) handles both, plus everything around them.

## Run it now (the interpreter)

```
lumen run hello.lm
```

This walks your program directly: no build step, instant feedback. It's the
right choice while you're writing code, and it's the *reference* implementation:
if the interpreter and compiler ever disagree, the interpreter is correct by
definition.

## Build a real executable

```
lumen build hello.lm                 # produces hello.exe next to the source
lumen build hello.lm -o myapp.exe    # choose the output name
./myapp.exe
```

`lumen build` emits actual x86-64 machine code, hands it to GCC to assemble and
link, and gives you a standalone native executable. No runtime to install, no
bytecode, just an `.exe`. This is what you ship, and what the
[benchmarks](performance.md) measure.

### Finding the C toolchain (you don't have to set PATH)

`lumen build` shells out to GCC (the MinGW-w64 toolchain on Windows) for the
final assemble-and-link step. You do **not** have to put it on your `PATH`
first. Lumen locates it for you, in this order:

1. an explicit override - `LUMEN_CC` (then the generic `CC`) pointing at a
   `gcc.exe`;
2. `gcc` already on `PATH`;
3. a scan of the standard install roots: MSYS2 (`C:\msys64\{mingw64,ucrt64,
   clang64}\bin`), standalone MinGW-w64 (`C:\mingw64\bin`, ...), the Scoop
   mingw app, and the rustup `*-gnu` toolchain.

The first hit wins, and Lumen puts that toolchain's own `bin` on the child
process's `PATH` so `gcc` can find its sibling `as`/`ld`. This is why a build
that fails from a fresh PowerShell (`could not run gcc`) just works once GCC is
installed anywhere standard - no shell configuration needed.

To see exactly what Lumen finds, run `lumen doctor`:

```
lumen doctor
```

```
Lumen 0.69.0 - toolchain check

  gcc      found    C:\msys64\mingw64\bin\gcc.exe  (via auto-detected)
  windres  found    C:\msys64\mingw64\bin\windres.exe  (via auto-detected)  [optional, for --icon]

Native builds are ready: `lumen build file.lm -o file.exe`
```

If no compiler is found, `doctor` prints install instructions (MSYS2 / winget /
scoop) and exits non-zero. To force a specific compiler, set `LUMEN_CC`:

```
set LUMEN_CC=C:\path\to\gcc.exe      # cmd
$env:LUMEN_CC = 'C:\path\to\gcc.exe' # PowerShell
```

> **PowerShell `&&` note:** `lumen build app.lm -o app.exe && .\app.exe` fails in
> *Windows PowerShell 5.x* with `The token '&&' is not a valid statement
> separator` - that's PowerShell, not Lumen. Use `;` (runs the second command
> regardless), upgrade to PowerShell 7+ (which supports `&&`), or run the two
> commands on separate lines.

A few features (notably FFI callbacks) only work in a compiled program, because
they need a real machine-code address to hand to the OS. The interpreter tells
you clearly when you've hit one.

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
lumen doctor                       check the native-build toolchain (gcc, windres)
lumen tokens file.lm               dump the token stream (for debugging)
lumen ast    file.lm               dump the parsed syntax tree (for debugging)
lumen version                      print the version
```

## The REPL

`lumen repl` gives you an interactive prompt that remembers your definitions and
variables between lines. To enter a multi-line block, end a line with `:` and
finish with a blank line:

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

You don't list every file to the compiler. Point it at the entry file and it
follows every `import` automatically:

```
lumen build myproject/main.lm -o app.exe
```

See [imports](../syntax/imports.md) for how modules resolve and combine.

## When things go wrong

Errors here are meant to be read, not decoded. Whether it's a parse error, a
compile-time type problem, or a runtime fault, Lumen prints a plain
`error: <message>` and points straight at the line responsible. Under `lumen run`
you get the offending source line with a caret tucked underneath it. A compiled
`.exe` doesn't carry your source text, so it points with `--> line N` instead.
Either way, you walk away with the message and the location.
