# Lumen documentation

Two ways in:

### [`lumen/`](lumen/): how the language *works*
The mental model: what Lumen is, how a program runs (and why two ways to run it
must agree to the byte), how memory works, where the speed comes from, how it
talks to the outside world, and what it can't do yet. Start here.

- [What Lumen is](lumen/overview.md)
- [Running and building a program](lumen/running.md)
- [The two backends (and the byte-identical contract)](lumen/two-backends.md)
- [How memory works](lumen/memory.md)
- [How it runs fast](lumen/performance.md)
- [Calling C libraries and DLLs (FFI)](lumen/ffi.md)
- [What it can't do yet](lumen/limitations.md)

### [`syntax/`](syntax/): how the *syntax* works
The reference: one page per feature with copy-pasteable examples. Reach for this
when you know what you want and just need the spelling.

- [The basics: layout, comments, `main`](syntax/basics.md)
- [Values and types](syntax/values.md)
- [Variables](syntax/variables.md)
- [Control flow](syntax/control-flow.md)
- [Functions](syntax/functions.md)
- [Errors: `try` / `catch` / `raise`](syntax/errors.md)
- [Strings](syntax/strings.md)
- [Lists](syntax/lists.md)
- [Maps](syntax/maps.md)
- [Structs and methods](syntax/structs.md)
- [Imports and multi-file programs](syntax/imports.md)
- [The standard library](syntax/stdlib.md)

---

Every example here runs identically under `lumen run` (interpreter) and
`lumen build` (native `.exe`). That's the core promise, enforced by the test
suite. If something here disagrees with the compiler, the docs are wrong; flag it.
