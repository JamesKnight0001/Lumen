# Imports and multi-file programs

`import` pulls another file's definitions into your program. A module is just a
`.lm` file of definitions, usually with no `main`.

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

- A plain name resolves to `<name>.lm` **in the importing file's directory**.
- A dotted path maps onto nested directories: `import pkg.util` loads `pkg/util.lm`.
- A **leading dot** makes the import *relative to the importing file's own
  directory*; each extra dot climbs one parent (Python-style):
  `import .util` (same dir), `import ..common` (parent dir),
  `import ...root.x` (two dirs up). `from .pkg import f` works too. This lets a
  module reach a sibling or parent without anchoring to the entry file.
- The same syntax brings in the built-in modules the runtime provides: `math`,
  `os`, `rand`, `json`, `time`, `net`, `cffi`. See
  [the standard library](stdlib.md).
- A plain name that is **not** a sibling or builtin is looked up in
  `lumen_modules/` (see [Packages](#packages-and-environments)).

## What import does and doesn't do

- It brings in the module's **functions, structs, and `extern` blocks**.
- It does **not** run the module's top-level statements; only definitions are
  pulled in. A module that defines a `main` won't run it on import.
- Each file is loaded **at most once**. That makes diamond imports (A imports B
  and C, which both import D) and import cycles safe: D shows up once.

## Building a multi-file project

You don't hand every file to the compiler. Point it at the entry file; every
`import` is followed automatically and merged into a single program:

```
lumen build myproject/main.lm -o app.exe
```

That merge happens once at parse time, so the interpreter and the native binary
compile the *identical* combined program. That keeps the two
[byte-identical](../lumen/two-backends.md).

## The one gotcha

Imported names currently share **one global namespace**, so two modules that
each define a top-level `foo` will collide. The fix: keep top-level names
distinct across modules, and lean on qualified calls (`greet.greeting(...)`),
which read more clearly. Per-module isolation is on the roadmap. See
[limitations](../lumen/limitations.md#imports-share-one-global-namespace).

Working multi-file code: `examples/09_imports.lm` and the runnable
`examples/project/`.

## Packages and environments

Beyond sibling files, Lumen can install packages: single `.lm` modules fetched
over HTTP into a `lumen_modules/` directory the import resolver searches after
siblings and builtins.

```
lumen install https://example.com/rng/rng.lm   # install one package by URL
lumen install rng                              # by name, via the registry
lumen install                                  # install everything in lumen.pkg
```

Resolution order for a plain `import name`:

1. a sibling `./name.lm` next to the importing file (unchanged);
2. the active virtual env's `lumen_modules/` (if `LUMEN_VENV` is set);
3. a project-local `lumen_modules/`.

### The manifest (`lumen.pkg`)

Installing by name or URL records the dependency under `[deps]`, so a later
bare `lumen install` is reproducible:

```
name = app
version = 0.1.0

[deps]
rng = https://example.com/rng/rng.lm
```

A package can declare transitive dependencies with a `#!dep <name>
<source>` comment near its top; `lumen install` follows them, de-duplicating
so diamonds and cycles terminate.

### Virtual environments

```
lumen venv ./venv          # create an isolated lumen_modules/
export LUMEN_VENV=...       # activate it (printed by `lumen venv`)
```

With a venv active, `lumen install` installs into it and `lumen run`/`lumen
build` resolve imports from it, keeping a project's dependencies separate.

### Registry

Bare-name installs resolve to `<registry>/<name>/<name>.lm`. Override the
default registry with the `LUMEN_REGISTRY` environment variable.

> Package downloads use an internal WinHTTP-backed client, so `install`/`update`
> are Windows-only today.
