# The basics: layout, comments, `main`

Before anything else, here's the shape every Lumen file shares. It's a short list.

## Indentation is the structure

Lumen uses the off-side rule, the same way Python does: a block opens after a
line ending in `:`, and the body is whatever's indented further than that line.
There are no curly braces and no `end` keyword. The indentation *is* the nesting,
which means the code can't lie to you about its own structure.

```lumen
fn classify(n):
    if n < 0:
        return "negative"
    elif n == 0:
        return "zero"
    else:
        return "positive"
```

Stay consistent within a block. Use spaces, and four of them is the convention.

## Statements end at the newline

No semicolons here. One statement per line, and the newline does the work.

```lumen
let x = 10
let y = 20
print(x + y)
```

## Comments start with `#`

Everything from `#` to the end of the line is ignored. Use them freely.

```lumen
let radius = 5        # in metres
# the next line computes the area
let area = 3.14159 * radius * radius
```

For a comment that spans several lines, wrap it in `#[ ... ]#`. It can run
across as many lines as you like, and also works inline in the middle of a line.

```lumen
#[ This is a block comment.
   It can span multiple lines without a `#` on each one. ]#
let x = 1 #[ inline note ]# + 2
```

## `main` is your entry point

Define a function called `main` and Lumen runs it for you, automatically. You
never call it yourself.

```lumen
fn main():
    print("this runs on its own")
```

A file *without* a `main` is a **module**: just a bag of definitions other files
can [import](imports.md). Importing pulls in its functions and types, but it does
**not** run any top-level code, so a module that happens to define `main` won't
run it when imported. That's the whole trick.

## A complete first program

```lumen
fn greet(name):
    return f"Hello, {name}!"

fn main():
    print(greet("world"))
```

```
$ lumen run greet.lm
Hello, world!
```

From here, pick a feature and dig in:
[values](values.md) · [variables](variables.md) ·
[control flow](control-flow.md) · [functions](functions.md) ·
[strings](strings.md) · [lists](lists.md) · [maps](maps.md) ·
[structs](structs.md) · [errors](errors.md) · [imports](imports.md) ·
[the standard library](stdlib.md).
