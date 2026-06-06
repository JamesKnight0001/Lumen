# What it can't do yet

Here's the honest list. None of these are secret traps waiting to bite you;
they're the known edges of a language that's kept small on purpose, written down
so you hit them on the page instead of at 2am.

## Integers are 48-bit

Because every value is packed into one 64-bit word, integers get a 48-bit
payload: exact in roughly ±1.4×10¹⁴, and **wrapping** (not trapping) on overflow.
The wrap is identical in both backends, so it's at least predictable. For larger
magnitudes, use floats. See [values](../syntax/values.md#integers-are-48-bit).

## No user-defined generics

Values are dynamically typed, and there's no generic-type system to write
against. In practice the dynamism covers most of what generics would buy you (a
list or map holds anything), but you can't write a statically-parameterized
container or function.

## Imports share one global namespace

`import` merges another file's definitions into one combined program. That keeps
the two backends in lockstep (they compile the identical merged code), but it
means two modules that both define a top-level `foo` will **collide**. The fix
today is simple: use distinct names across modules, and prefer qualified calls
(`greet.greeting(...)`), which read clearly anyway. Per-module name isolation is
planned. See [imports](../syntax/imports.md).

## No incremental compilation

Every `lumen build` recompiles the whole program from scratch. For the program
sizes Lumen targets this is fast enough not to notice, but unchanged modules
aren't cached.

## FFI callbacks are compiled-only

`cffi.callback` needs a real machine-code address to give the operating system,
which the tree-walking interpreter doesn't have. So callback-based programs
(anything with a `WndProc`, an `EnumWindows` proc, etc.) must run with `lumen
build`, not `lumen run`. The interpreter detects this and prints a clear message
pointing you to `lumen build` rather than failing mysteriously. The deterministic
pieces around callbacks (reading/writing memory through pointers) still work in
both backends. See [FFI](../lumen/ffi.md).

## The FFI's other edges

- The interpreter handles up to 4 foreign-call arguments; the native backend
  handles up to 16. Wide-signature calls (like `CreateWindowExA`) therefore want
  a compiled build.
- COM and callback support is shaped for the common Windows cases (which is
  enough to reach DirectX). Exotic calling conventions or very large struct
  returns may need new primitives.

---

Everything *not* on this list, every [syntax](../syntax/) feature and the whole
[standard library](../syntax/stdlib.md), is pinned down by the interp-vs-native
test suite and behaves identically on both backends. That's the deal.
