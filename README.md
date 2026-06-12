 # Lumen - BETA V0.78.0

A small, fast language with Python-like syntax and a Lua-sized core.

Lumen can either:

* **Interpret** source (`lumen run`)
* **Compile** directly to native **x86-64 executables** (`lumen build`)

Two compiled backends share one runtime and produce byte-identical output: an
LLVM backend (the default - emits LLVM IR, links with clang + lld) and a
hand-written x86-64 assembly backend (the fallback when no LLVM toolchain is
present, opt-in via `--backend asm`).

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
* An LLVM toolchain (`clang` + `lld`) for the default build backend, **or** GCC
  for the fallback asm backend. On Windows, install MinGW-w64 / LLVM (e.g. via
  MSYS2: `pacman -S mingw-w64-x86_64-clang mingw-w64-x86_64-lld`) anywhere
  standard and Lumen finds it automatically - no PATH setup needed. Run
  `lumen doctor` to confirm, or set `LUMEN_CLANG` / `LUMEN_CC` to point at a
  specific binary.

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
| `lumen build <file>`  | Compile to a native executable (LLVM by default; `--backend asm` for gcc) |
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
      ├─ LLVM backend      (default: .ll → clang + lld → .exe)
      └─ x86-64 backend    (fallback / --backend asm: GAS → gcc)
```

Both execution modes consume the same lowered AST. A differential test suite
verifies that interpreted and compiled programs produce identical output, down
to the byte.

## Performance

`lumen build` compiles to native code via LLVM (`-O3`). Values are NaN-boxed,
and a whole-program type analysis proves which locals, parameters, and list
elements are always integers or always floats, so they stay **unboxed** in the
generated code - integer/float arithmetic and comparisons become inline machine
instructions instead of runtime calls, and tight numeric loops drop their GC
safepoints entirely.

Four workloads, each computing an identical result in every language, best-of-5
wall-clock. Measured on this machine (Windows; LLVM/clang 21.1.6, gcc 16.1,
rustc 1.96, OpenJDK 25, Node 24, CPython 3.14). `fib`/`loop`/`map` outputs are
byte-identical across all six languages, and Lumen's interpreter, LLVM build,
and asm build agree to the byte. Run it yourself: `python bench/bench.py`.

| Workload | Lumen | C `-O2` | Rust `-O` | Java | Node | Python |
|----------|------:|--------:|----------:|-----:|-----:|-------:|
| `fib(35)` recursion        | **50 ms**  | 19 ms | 27 ms | 86 ms  | 155 ms | 1045 ms |
| float reduce, 5×10⁷ iters  | **77 ms**  | 52 ms | 53 ms | 166 ms | 102 ms | 3900 ms |
| int loop + modulo, 5×10⁷   | **82 ms**  | 50 ms | 53 ms | 116 ms | 148 ms | 4030 ms |
| hash-map build+lookup, 10⁶ | **31 ms**  | 12 ms | 27 ms | 98 ms  |  84 ms |  144 ms |

Ratios vs C (lower is better): Lumen is **1.5×** off C on the float reduction,
**1.6×** on the integer loop, and **2.6×** on recursion and hash-map throughput.
It beats Java, Node, and CPython on every workload - up to ~3× faster than Node
and 12-80× faster than CPython - while staying a dynamically-typed language with
the same source running unchanged under the interpreter.

Read those honestly: C and Rust still win outright, and on some reductions they
auto-vectorize or constant-fold work away that Lumen emits as straight scalar
code. The goal isn't to beat C - it's to get within a small constant factor of
it from a Python-like language, and to leave every managed runtime behind. The
full methodology and per-release deltas live in
[`docs/lumen/performance.md`](docs/lumen/performance.md).

## Status

Current features include:

* Two native backends: LLVM (default) and hand-written x86-64 assembly
* Generational mark/sweep garbage collection
* Closures
* Modules, relative imports, default arguments
* C FFI (including Win32/COM usage)
* Standard library

Recent backend work:

* **LLVM backend** - `lumen build` defaults to emitting LLVM IR and linking with
  clang + lld; falls back to the asm backend when no LLVM toolchain is present
* **Unboxed numerics** - proven int/float locals compile to inline `i64`/`double`
  arithmetic instead of boxed runtime calls (recursion ~2.9×, float/loop ~4× faster)
* **Branch-on-compare** - int/float comparisons feeding `if`/`while` lower to a
  direct `icmp`/`fcmp` + branch, skipping the box/unbox round-trip
* **GC safepoint elision** - loops proven not to allocate drop their per-iteration
  GC poll

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
