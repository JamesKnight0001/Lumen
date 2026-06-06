# Imports and multi-file programs

`import` pulls another file's definitions into your program. A module is nothing
fancy: it's just a `.lm` file full of definitions, usually with no `main` of its
own.

## A module

```lumen
# greet.lm - definitions, no main()
fn greeting(name):
    return f"Hello, {name}!"

fn shout(s):
    return s.upper()
```

## The four ways to import it

```lumen
import greet                  # qualified:  greet.greeting("x")
import greet as g             # aliased:    g.greeting("x")
import pkg.util               # nested dir: util.foo(...)   (loads pkg/util.lm)
from greet import shout       # selective:  shout("x")      (unqualified)

import math                   # built-in modules use the same syntax
```

- A plain name resolves to `<name>.lm` **in the same directory as the importing
  file**.
- A dotted path maps onto nested directories: `import pkg.util` loads `pkg/util.lm`.
- The same syntax brings in the built-in modules, `math`, `os`, `rand`, `json`,
  `time`, `cffi`, that the runtime provides. See
  [the standard library](stdlib.md).

## What import does and doesn't do

- It brings in the module's **functions, structs, and `extern` blocks**.
- It does **not** run the module's top-level statements; only the definitions are
  pulled in. So a module that happens to define a `main` won't run it on import.
- Each file is loaded **at most once**. That's what makes diamond imports (A
  imports B and C, which both import D) and import cycles safe: D shows up just
  once.

## Building a multi-file project

You don't hand every file to the compiler. Just point it at the entry file, and
every `import` gets followed automatically and merged into a single program:

```
lumen build myproject/main.lm -o app.exe
```

Because that merge happens just once at parse time, the interpreter and the
native binary compile the *identical* combined program. That's what keeps the two
[byte-identical](../lumen/two-backends.md).

## The one gotcha

There's one thing to watch out for. Imported names currently share **one global
namespace**, so two modules that each define a top-level `foo` will collide. The
fix is simple: keep top-level names distinct across modules, and lean on qualified
calls (`greet.greeting(...)`), which read more clearly anyway. Per-module isolation
is on the roadmap. See
[limitations](../lumen/limitations.md#imports-share-one-global-namespace).

For working multi-file code, see `examples/09_imports.lm` and the runnable
`examples/project/`.
