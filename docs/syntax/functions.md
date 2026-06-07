# Functions

```lumen
fn add(a, b):
    return a + b
```

Define with `fn`, take some parameters, `return` a value. Or don't: a function
that falls off the end just returns `nil`. You can call a function from anywhere
in the file, because **declaration order doesn't matter**, so mutually recursive
functions are no trouble at all.

## Expression-bodied form

When the whole body is a single expression, you can skip the block entirely:

```lumen
fn square(n) = n * n
fn greet(name) = f"Hello, {name}!"
```

## Any number of parameters

```lumen
fn sum6(a, b, c, d, e, f):
    return a + b + c + d + e + f
```

There's no special limit to keep in mind. Pass as many as you like.

## Functions are values

A bare function name (no parentheses) *is* a value. You can pass functions to
other functions, stash them in lists and maps, return them, and call them
indirectly. This is the foundation everything else builds on: `map`/`filter`,
dispatch tables, the lot.

```lumen
fn add(a, b): return a + b
fn mul(a, b): return a * b

fn apply(f, x, y):
    return f(x, y)               # call through a parameter

fn main():
    print(apply(add, 3, 4))      # 7
    let ops = [add, mul]
    print(ops[0](10, 5))         # 15   (a function pulled out of a list)
    let table = {"+": add, "*": mul}
    print(table["*"](8, 2))      # 16   (a dispatch table)
    print(type(add))             # "fn"
```

## Anonymous functions and closures

Write a function inline with `fn(params): expr` (a single expression) or with an
indented block body. The interesting part is that anonymous functions **capture**
the variables around them:

- **by value** if the captured variable is never reassigned, or
- **by reference** (shared, mutable) if it is,

which means stateful closures behave the way you'd hope they would:

```lumen
fn make_counter():
    let count = 0
    return fn():
        count = count + 1
        return count

fn main():
    let c = make_counter()
    print(c())     # 1
    print(c())     # 2
    let d = make_counter()
    print(d())     # 1   (each counter keeps its own independent count)
```

Pair closures with [`list.map` and `list.filter`](lists.md) for compact
transformations  the `fn(...)` literal can go straight into the method call:

```lumen
let evens = [1, 2, 3, 4, 5, 6].filter(fn(v): v % 2 == 0)   # [2, 4, 6]
let squares = [1, 2, 3, 4].map(fn(v): v * v)               # [1, 4, 9, 16]
```

## A small FFI footnote

Handing a Lumen function to the operating system as a *callback* (a `WndProc`,
say) requires a compiled program. See [FFI](../lumen/ffi.md) for the details.
Ordinary Lumen-to-Lumen calls have no such restriction.
