# Lumen - BETA V0.79.0

A small, fast language with Python-like syntax and a Lua-sized core.

Two ways to run:

* **Interpret** source (`lumen run`)
* **Compile** to native **x86-64 executables** (`lumen build`)

Both compiled backends share one runtime and emit byte-identical output: an LLVM
backend (default: LLVM IR, linked with clang + lld) and a hand-written x86-64
assembly backend (fallback when no LLVM toolchain is present, or `--backend asm`).

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

## Install

Two clicks with the [Lumen installer](https://github.com/JamesKnight0001/Lumen_installer)
(VS Code syntax highlighting included).

### From source

Needs Rust (via rustup) and either an LLVM toolchain (`clang` + `lld`) for the
default backend or GCC for the asm fallback. On Windows, install MinGW-w64 / LLVM
anywhere standard (e.g. `pacman -S mingw-w64-x86_64-clang mingw-w64-x86_64-lld`)
and Lumen finds it automatically. Run `lumen doctor` to confirm, or set
`LUMEN_CLANG` / `LUMEN_CC` to a specific binary.

```sh
cd compiler
cargo build --release
```

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
* Structs and methods (`impl`), and `enum` types
* `if`, `while`, `for`, `break`, `continue`, and `match` / `case`
* List and pair destructuring (`let a, b = pair`, `for k, v in m.items()`)
* Modules and imports, including relative imports (`.mod` / `..mod`)
* Default function arguments
* f-strings
* Native C FFI via `extern`
* TCP/UDP sockets (`net`) and a package manager (`lumen install`, `lumen venv`)

Built-in modules: `math`, `os`, `json`, `rand`, `time`, `net`.

## Architecture

```text
source
  -> lexer
  -> parser
  -> import resolver
  -> optimizer (runs to a fixpoint)
  -> type analysis (proves int / float / int-list / float-list)
      ├─ interpreter
      ├─ LLVM backend      (default: .ll -> clang + lld -> .exe)
      └─ x86-64 backend    (fallback / --backend asm: GAS -> gcc)
```

Both modes consume the same lowered AST. A differential test suite verifies that
interpreted and compiled programs produce byte-identical output.

## Performance

`lumen build` compiles via LLVM (`-O3`). Values are NaN-boxed, but a whole-program
analysis proves which locals, params, and list elements are always int or always
float and keeps them **unboxed**: arithmetic and comparisons become inline machine
instructions, proven-int locals stay raw `i64` in their slots (no per-iteration
box/unbox), non-escaping collections are arena-allocated off the GC heap, and
allocation-free loops drop their GC safepoints.

Four workloads, identical result in every language, best-of-5 wall-clock. Measured
here on Windows (LLVM/clang 21.1.6, gcc 16.1, rustc 1.96, OpenJDK 25, Node 24,
CPython 3.14). `fib`/`loop`/`map` outputs match across all six languages, and
Lumen's interpreter, LLVM build, and asm build agree to the byte. Run it yourself:
`python bench/bench.py`.

| Workload | Lumen | C `-O2` | Rust `-O` | Java | Node | Python |
|----------|------:|--------:|----------:|-----:|-----:|-------:|
| `fib(35)` recursion        | **57 ms**  | 22 ms | 30 ms | 99 ms  | 177 ms | 1212 ms |
| float reduce, 5×10⁷ iters  | **56 ms**  | 54 ms | 56 ms | 184 ms | 111 ms | 4684 ms |
| int loop + modulo, 5×10⁷   | **60 ms**  | 56 ms | 58 ms | 133 ms | 168 ms | 4900 ms |
| hash-map build+lookup, 10⁶ | **57 ms**  | 15 ms | 46 ms | 123 ms |  111 ms |  170 ms |

The float reduction (~1.03×) and integer loop (~1.1×) are effectively tied with C;
recursion is ~2.6× and hash-map throughput ~3×. Lumen beats Java, Node, and CPython
on every workload (up to ~3× faster than Node, 12-85× faster than CPython).

C and Rust still win on recursion (lower call overhead) and hash-maps, and on some
reductions they auto-vectorize or constant-fold work that Lumen emits as scalar
code. The goal isn't to beat C, it's to get within a small constant factor from a
Python-like language while leaving every managed runtime behind. Full methodology
in [`docs/lumen/performance.md`](docs/lumen/performance.md).

## Status

Shipping:

* Two native backends: LLVM (default) and hand-written x86-64 assembly
* Generational mark/sweep GC
* Closures
* Modules, relative imports, default arguments
* C FFI (including Win32/COM)
* Standard library

Recent backend work:

* **LLVM backend** - default; emits LLVM IR + clang/lld, falls back to asm
* **Unboxed numerics** - proven int/float locals use inline `i64`/`double` math
* **Raw-int slots** - proven-int locals stay raw `i64` across a loop (int loop ~1.1× C)
* **Arena allocation** - non-escaping lists/maps/structs/comprehensions are freed
  on return, off the GC heap (Rust-like scope-bound freeing, no annotations)
* **Pure-helper attributes** - `memory(none)` lets LLVM drop dead boxes (float ~1.03× C)
* **Branch-on-compare** - int/float compares feeding `if`/`while` lower to `icmp`/`fcmp` + branch
* **GC safepoint elision** - allocation-free loops drop their per-iteration poll

Limitations:

* x86-64 only
* 48-bit wrapping integers
* No incremental compilation
* Imported modules share a global namespace
* No auto-vectorizer (the main remaining gap to C on reducible loops)

## Documentation

* `docs/lumen/` - language guide
* `docs/syntax/` - reference
* `examples/` - language tour and sample projects

## Credits

Thanks to the friends who bug-found with it.

## Author

James Phifer [JamesKnight0001]

## License

MIT
