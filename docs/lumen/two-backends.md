# The two backends (and the byte-identical contract)

If you understand one thing about how Lumen works, make it this.

Every Lumen program can run two completely different ways:

1. **The interpreter** (`lumen run`) is a tree-walker. It reads your program into
   a syntax tree and walks it, doing whatever each node says. Simple, easy to
   trust, easy to reason about. Slow-ish, but never wrong.
2. **The native compiler** (`lumen build`) emits real x86-64 assembly, which GCC
   turns into a `.exe`. Fast, standalone, no interpreter anywhere in sight at
   runtime.

Two completely separate implementations of the same language. So what stops them
from drifting apart? One rule, and the whole project is built around it:

> **For any program, the interpreter and the compiled binary must print
> byte-for-byte identical output.**

## Why this matters

Most languages with two execution paths eventually drift. The fast path picks up
a subtle quirk the reference path doesn't have, and suddenly your program does
one thing under the debugger and another in production. That's a miserable bug to
chase, because the thing you're testing isn't the thing that's running.

Lumen just refuses to allow it. Any disagreement between the two is a compiler
bug, full stop. The interpreter is the source of truth for what a program
*means*; the compiler's only job is to mean the same thing, faster. The test
suite runs every example through both backends and diffs the output. One byte
off and the build is red.

In practice that buys you a nice workflow: iterate with `lumen run` (instant,
friendly errors with a caret under the offending token), ship with `lumen build`
(native speed), and never wonder whether switching changed your program's
behavior. It didn't. It can't.

## What counts as identical (and what doesn't)

The contract covers **stdout**, what your program actually prints. A few things
sit outside it, because no honest contract could include them:

- **Error styling on stderr** differs a little. The interpreter has your source,
  so it draws a caret under the bad line; the compiled `.exe` doesn't, so it says
  `--> line N`. The message and the line number match. The decoration doesn't,
  and that's fine.
- **Genuinely non-deterministic things**: wall-clock time (`time.now()`,
  `os.clock()`), the process id, the working directory, whatever an external
  command prints (`os.system` / `os.exec`), and anything driven by OS events like
  windows or callbacks. These vary by their nature, so they stay out of the
  conformance suite. Seeded randomness is the exception worth remembering:
  `rand.seed(n)` gives the same sequence on both backends, so seeded programs are
  still checkable to the byte.

Everything else is identical on purpose: arithmetic (down to how integer overflow
wraps), string formatting, map iteration order, the way floats print.

## A consequence worth knowing

Both backends compile the *same* merged program, so multi-file programs, diamond
imports, and import cycles all behave the same under each. The merge happens once,
at parse time. There's simply no "the interpreter saw a different program than the
compiler did" class of bug to worry about.
