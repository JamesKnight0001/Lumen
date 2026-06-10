 # Lumen - BETA V0.78.0

A small, fast language with Python-like syntax and a Lua-sized core.

Lumen can either:

* **Interpret** source (`lumen run`)
* **Compile** directly to native **x86-64 executables** (`lumen build`)

No LLVM. No external IR. Just a parser, optimizer, interpreter, and native code generator.

```lumen
fn make_counter():
    let count = 0
    return fn():
        count = count + 1
        return count

fn main():
    let next = make_counter()
    print(next())          # 1
    print(next())          # 2
    print(2 ** 10)         # 1024
    print("ell" in "hello")
```

## Version
Compiler: V0.78.0

# Install using Installer:
[Lumen installer](https://github.com/JamesKnight0001/Lumen_installer)
All it takes is 2 clicks, to install lumen.
[Lumen Syntax Highlight included for VsCode]

## local Install [compiler]

Requirements:

* Rust (via rustup)
* GCC/Clang (used for assembling and linking compiled programs). On Windows,
  install MinGW-w64 (e.g. via MSYS2) anywhere standard and Lumen finds it
  automatically - no PATH setup needed. Run `lumen doctor` to confirm, or set
  `LUMEN_CC` to point at a specific `gcc.exe`.

Build:

```sh
cd compiler
cargo build --release
```

Run:

```sh
lumen run examples/01_hello.lm
lumen build examples/01_hello.lm -o hello.exe
./hello.exe

lumen repl
lumen -c 'fn main(): print(2 ** 10)'
```

## Commands

| Command               | Description                    |
| --------------------- | ------------------------------ |
| `lumen run <file>`    | Interpret a program            |
| `lumen build <file>`  | Compile to a native executable |
| `lumen repl`          | Interactive REPL               |
| `lumen -c "<src>"`    | Run inline source              |
| `lumen check <file>`  | Parse and compile-check        |
| `lumen emit <file>`   | Emit generated assembly        |
| `lumen install [pkg]` | Install packages into `lumen_modules/` |
| `lumen venv <dir>`    | Create an isolated package environment |
| `lumen update`        | Download + install a newer compiler |
| `lumen doctor`        | Check the native-build toolchain (gcc, windres) |
| `lumen ast <file>`    | Print AST                      |
| `lumen tokens <file>` | Print token stream             |

## Language Features

* Integers, floats, strings, lists, maps, structs, booleans, `nil`
* Functions, closures, recursion
* Structs and methods (`impl`)
* `if`, `while`, `for`, `break`, `continue`
* Modules and imports - including relative imports (`.mod` / `..mod`)
* Default function arguments
* f-strings
* Native C FFI via `extern`
* TCP/UDP sockets (`net`) and a package manager (`lumen install`, `lumen venv`)

Built-in modules include `math`, `os`, `json`, `rand`, `time`, and `net`.

## Architecture

```text
source
  → lexer
  → parser
  → import resolver
  → optimizer (runs to a fixpoint)
  → type analysis (proves int / float / int-list / float-list)
      ├─ interpreter
      └─ x86-64 code generator
```

Both execution modes consume the same lowered AST. A differential test suite
verifies that interpreted and compiled programs produce identical output, down
to the byte.

## Performance

Compiled Lumen is built to meet C on the hot numeric path and to beat every
managed language (Java, Node, CPython) across the board. Values are NaN-boxed,
and the compiler proves which locals, parameters, and list elements are always
integers or always floats so they stay **unboxed** in the generated machine
code. Reduction accumulators and loop counters are promoted into registers.

Measured on this machine (Windows, mingw64 gcc 15.2, rustc 1.96, Java 25,
Node 24, Python 3.12); medians, warmup discarded; every program's output is
byte-identical across languages:

| Workload | Lumen 0.77 | C `-O2` | Rust `-O` | Node | Python |
|----------|-----------|---------|-----------|------|--------|
| `fib(34)` recursion | **~32 ms** | ~15 ms | ~21 ms | ~117 ms | ~1132 ms |
| collatz (data-dependent scalar) | **~139 ms** | ~52 ms | ~35 ms | ~282 ms | ~3220 ms |
| int-list sum, 100M reads | **~225 ms** | ~21 ms* | ~15 ms* | ~182 ms | ~8975 ms |

Read those honestly. On fair scalar code Lumen lands ~2-2.7× off C and clearly
ahead of Node (2-4×) and Python (20-35×). The asterisks matter: on the int-list
sum, gcc/rustc *auto-vectorize* (SIMD) and on simpler reductions they delete the
loop entirely via constant-folding - that's the compiler removing the work, not
the language being faster at it. Lumen emits honest scalar code and still beats
every interpreter. The full story, methodology, and per-release deltas are in
[`docs/lumen/performance.md`](docs/lumen/performance.md).

## Status

Current features include:

* Native x86-64 code generation
* Generational mark/sweep garbage collection
* Closures
* Modules, relative imports, default arguments
* C FFI (including Win32/COM usage)
* Standard library

Recent performance work (0.76-0.78):

* **S3** - int-list `a[i]` reads compile to an inline unboxed load (**+60%** on
  int-list-heavy loops)
* **S5** - the optimizer now runs inline→fold→CSE→DCE to a fixpoint
* **S7** - interpreter interns string literals (**+21%** on literal-heavy loops)
* **S10** - int reduction accumulators are promoted to registers in `for` loops
  (**+23%** on accumulator loops)
* **S11** - `x % 2^k` compiles to a sign-correct mask instead of a divide
  (**+19%** on modulo-heavy loops like collatz)

Current limitations:

* x86-64 only
* 48-bit wrapping integers
* No incremental compilation
* Imported modules share a global namespace
* No auto-vectorizer (the main remaining gap to C on reducible loops)

## Documentation

* `docs/lumen/` - language guide
* `docs/syntax/` - reference documentation
* `examples/` - language tour and larger sample projects

## Credits
Thanks to friends who used the language to bug find!

## Author
By James Phifer [JamesKnight0001]

## License
MIT
