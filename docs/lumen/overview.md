# What Lumen is

Lumen is a small, fast language. If you've written Python, you can read it on
sight: significant indentation, `fn` instead of `def`, no semicolons, no curly
braces. Underneath, it's a systems language. `lumen build` turns your program
into a native `.exe` of machine code, not bytecode for a virtual machine to run
later.

The design follows one idea: a tiny core that does a few things well and fast,
not a big language that does everything. Python's feel, Lua's size, a compiler's
output.

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

That's the shape of it: `let` names things, indentation marks blocks, `for i in
1..101` is a half-open range, f-strings interpolate, and `main()` runs on its own
(you define it; you never call it).

## What's in the box

- **The values you expect**: integers, floats, booleans, strings, lists, maps,
  structs, and `nil`. Dynamically typed: you don't annotate your own variables
  (though FFI declarations name C types).
- **The control flow you expect**: `if` / `elif` / `else`, `while`, `for`,
  `break`, `continue`, a ternary `x if cond else y`, and list comprehensions.
- **Functions as real values**: pass them around, store them in lists and maps,
  return them. Closures capture their surroundings (by value, or by reference
  when mutated), so stateful counters just work.
- **Errors** with `try` / `catch` / `raise`.
- **A practical standard library**: `math`, `os` (files + environment), `rand`,
  `time`, `json`, and `cffi` (for calling into C/DLLs).
- **A real FFI**: call C functions in any DLL, pass structs, invoke COM methods
  (how Lumen reaches DirectX), and hand the OS a Lumen function as a callback.

## What makes it unusual

Lumen has **two execution engines contractually required to agree**:

- `lumen run` walks your program with a tree-walking interpreter.
- `lumen build` compiles it to native x86-64 and links a real executable.

Both must produce *byte-for-byte identical* output for every program. That's not
a nice-to-have; it's the project's correctness rule, checked by the test suite on
every change. The interpreter is the easy-to-trust reference; the compiler is the
fast path; switching never changes what your program does. The
[two-backends](two-backends.md) page has the full story.

## Where to go next

- New here? Keep reading: [running a program](running.md), then
  [the two backends](two-backends.md).
- Want the syntax? Jump to [`../syntax/`](../syntax/).
- Curious how it's fast, or how memory works? See
  [performance](performance.md) and [memory](memory.md).
