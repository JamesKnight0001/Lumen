# What it can't do yet

Here's the honest list. None are hidden traps, just the known edges of a
deliberately small language, written down so you meet them here, not at 2am.

## Integers are 48-bit

Every value packs into one 64-bit word, so integers get a 48-bit payload: exact
in roughly ±1.4×10¹⁴, **wrapping** (not trapping) on overflow. Both backends wrap
identically, so it's predictable. For larger magnitudes, use floats. See
[values](../syntax/values.md#integers-are-48-bit).

## No user-defined generics

Values are dynamically typed; there's no generic-type system. The dynamism covers
most of what generics would buy you (a list or map holds anything), but you can't
write a statically-parameterized container or function.

## Imports share one global namespace

`import` merges another file's definitions into one combined program. That keeps
both backends in lockstep (they compile identical merged code), but two modules
that both define a top-level `foo` will **collide**. The fix: use distinct names
across modules, and prefer qualified calls (`greet.greeting(...)`). Per-module
name isolation is planned. See [imports](../syntax/imports.md).

## No incremental compilation

Every `lumen build` recompiles the whole program from scratch. For Lumen's target
program sizes, that's fast enough not to notice (the compiler front-end is about
1% of build time, gcc is the other 99%), but unchanged modules aren't cached.

## No auto-vectorizer

The compiler emits honest scalar machine code. It doesn't generate SIMD or
constant-fold loops into closed form. Production C and Rust do both, so they look
dramatically faster on reducible numeric loops: they've turned the loop into a few
SIMD instructions, or deleted it. On scalar, data-dependent work, Lumen stays
within ~2-3× of C. Closing that gap means a
vectorizer (large) or unboxing proven-int locals out of NaN-boxing (also large);
both are on the speed roadmap, neither is done. See [performance](performance.md).

## FFI callbacks are compiled-only

`cffi.callback` needs a real machine-code address for the operating system, which
the tree-walking interpreter doesn't have. So callback-based programs (anything
with a `WndProc`, an `EnumWindows` proc, etc.) must run under `lumen build`, not
`lumen run`. The interpreter detects this and points you to `lumen build`. The
deterministic pieces (reading and writing memory through pointers) still work in
both backends. See [FFI](../lumen/ffi.md).

## The FFI's other edges

- The interpreter handles up to 4 foreign-call arguments, the native backend up
  to 16. Wide-signature calls (like `CreateWindowExA`) want a compiled build.
- COM and callback support targets the common Windows cases (enough to reach
  DirectX). Exotic calling conventions or large struct returns may need new
  primitives.

---

Everything *not* on this list, every [syntax](../syntax/) feature and the whole
[standard library](../syntax/stdlib.md), is pinned by the interp-vs-native test
suite and behaves identically on both backends. That's the deal.
