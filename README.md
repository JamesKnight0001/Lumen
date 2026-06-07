# Lumen

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
Compiler: V0.70.0

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
| `lumen doctor`        | Check the native-build toolchain (gcc, windres) |
| `lumen ast <file>`    | Print AST                      |
| `lumen tokens <file>` | Print token stream             |

## Language Features

* Integers, floats, strings, lists, maps, structs, booleans, `nil`
* Functions, closures, recursion
* Structs and methods (`impl`)
* `if`, `while`, `for`, `break`, `continue`
* Modules and imports
* f-strings
* Native C FFI via `extern`

Built-in modules include `math`, `os`, `json`, `rand`, and `time`.

## Architecture

```text
source
  → lexer
  → parser
  → import resolver
  → optimizer
  → type analysis
      ├─ interpreter
      └─ x86-64 code generator
```

Both execution modes consume the same lowered AST. A differential test suite verifies that interpreted and compiled programs produce identical output.

## Performance

Compiled Lumen code is designed to be competitive with C on numeric workloads and substantially faster than typical interpreted languages. Values are NaN-boxed, and proven integer/float values can remain unboxed in generated machine code.

## Status

Current features include:

* Native x86-64 code generation
* Garbage collection
* Closures
* Modules
* C FFI (including Win32/COM usage)
* Standard library

Current limitations:

* x86-64 only
* 48-bit wrapping integers
* No incremental compilation
* Imported modules share a global namespace

## Documentation

* `docs/lumen/` - language guide
* `docs/syntax/` - reference documentation
* `examples/` - language tour and larger sample projects

## Author
By James Phifer [JamesKnight0001]

## License
MIT
