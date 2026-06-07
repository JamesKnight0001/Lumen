# What Lumen is

Lumen is a small, fast programming language. If you've written Python, you can
read it on sight: significant indentation, `fn` instead of `def`, no semicolons,
no curly braces. Underneath, though, it behaves like a systems language.
`lumen build` turns your program into a real native `.exe` full of machine code,
not bytecode for some virtual machine to chew on later.

The whole design follows one idea, kept deliberately small: a tiny core that does
a few things well and fast, instead of a big language that tries to do
everything. Think Python's feel, Lua's size, a compiler's output.

## The thirty-second tour

```lumen
fn main():
    let total = 0
    for i in 1..101:
        total += i
    print(f"1 + 2 + ... + 100 = {total}")
```

```
$ lumen run sum.lm
1 + 2 + ... + 100 = 5050
```

That's the shape of it: `let` to name things, indentation for blocks, `for i in
1..101` over a half-open range, f-strings for interpolation, and a `main()` that
runs on its own (you define it; you never call it).

## What's in the box

- **The values you expect**: integers, floats, booleans, strings, lists, maps,
  structs, and `nil`. Dynamically typed, so you don't annotate your own variables
  (though FFI declarations do name C types).
- **The control flow you expect**: `if` / `elif` / `else`, `while`, `for`,
  `break`, `continue`, a ternary `x if cond else y`, and list comprehensions.
- **Functions as real values**: pass them around, store them in lists and maps,
  return them. Closures capture their surroundings (by value, or by reference
  when the variable is mutated), so stateful counters just work.
- **Errors** with `try` / `catch` / `raise`.
- **A practical standard library**: `math`, `os` (files + environment), `rand`,
  `time`, `json`, and `cffi` (for calling into C/DLLs).
- **A real FFI**: call C functions in any DLL, pass structs, invoke COM methods
  (this is how Lumen reaches DirectX), and hand the OS a Lumen function as a
  callback.

## What makes it unusual

Lumen has **two execution engines that are contractually required to agree**:

- `lumen run` walks your program with a tree-walking interpreter.
- `lumen build` compiles it to native x86-64 and links a real executable.

Both must produce *byte-for-byte identical* output for every program. That's not
a nice-to-have; it's the project's correctness rule, checked by the test suite on
every change. The interpreter is the easy-to-trust reference; the compiler is the
fast path; and switching between them never changes what your program does. The
[two-backends](two-backends.md) page explains why this matters and how it holds
up.

## Where to go next

- New here? Keep reading: [running a program](running.md), then
  [the two backends](two-backends.md).
- Want the syntax? Jump to [`../syntax/`](../syntax/).
- Curious how it's fast, or how memory is handled? See
  [performance](performance.md) and [memory](memory.md).
