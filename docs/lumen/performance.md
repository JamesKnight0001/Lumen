# How it runs fast

Lumen looks like a scripting language. A compiled Lumen program is not. It keeps
pace with C and Rust on the right workloads and leaves Java, Node, and CPython
well behind. Here's how, in plain terms. You don't *do* any of this; the compiler
just does it for you.

## 1. Values don't touch the heap

Every value is one 64-bit word ([NaN-boxing](memory.md)), and integers, floats,
booleans, and nil live right inside it. Number-crunching allocates nothing.
Copies are free. There's no object header to chase and no pointer to follow just
to read a loop counter.

## 2. It emits real machine code

`lumen build` doesn't hand a virtual machine some bytecode to chew through at
runtime. It writes x86-64 assembly, and GCC turns that into a native executable.
No dispatch loop, no "which opcode is this" check on every operation. Just
instructions the CPU runs directly.

## 3. A thin, optimized C runtime

A few things genuinely need real code: heap allocation, the GC, the string and
list helpers. Those live in a small C runtime, compiled at `-O2` and linked
straight into your program. Lean on purpose.

## 4. Unboxed numeric fast paths (the big one)

This is where most of the speed comes from. Before it generates any code, the
compiler runs a whole-program analysis that proves which variables and parameters
are *always* integers or *always* floats. Those skip boxing entirely:

- A proven-integer local lives as a raw `i64` in a register or stack slot, and
  `a + b` compiles to a bare `add`/`imul`. No tag check, no runtime call.
- A proven-float local lives in an SSE register; arithmetic becomes `addsd` /
  `mulsd` directly.
- A list proven to hold only floats has its `a[i]` reads compiled to a direct
  `movsd` from contiguous memory.
- Self-recursive tail calls become plain loops, and small functions get inlined.
- A recursive function's base case can return *before* its stack frame is even
  built, which on something like `fib` is roughly half of all calls.

## What that buys you

From `python bench/bench.py` (best-of-5; the int and `fib` outputs match
byte-for-byte across every language, because it's the same program):

| Workload                  | Lumen native | C `-O2` | Rust `-O` | Java | Node | Python |
|---------------------------|--------------|---------|-----------|------|------|--------|
| `fib(35)` (recursion)     | **~45 ms**   | ~22 ms  | ~29 ms    | ~85 ms | ~140 ms | ~1800 ms |
| float reduce, 5×10⁷ iters | **~52 ms**   | ~52 ms  | ~53 ms    | ~160 ms | ~89 ms | ~3600 ms |
| int loop `+= i%7`         | **~75 ms**   | ~50 ms  | ~53 ms    | ~114 ms | ~136 ms | ~4300 ms |
| map build+lookup, 10⁶ ops | **~37 ms**   | ~12 ms  | ~27 ms    | ~96 ms | ~70 ms | ~171 ms |

Read those honestly. On the float reduction, Lumen ties C outright. On recursion
it's about 2× C, and still ahead of Java and Node. The integer loop lands around
1.5× C. The map workload beats Rust, Java, Node, and Python. The goal was always
"meet C on the hot numeric path, beat every managed language," and the numbers
hold it up. It is *not* a claim of general parity with C everywhere, and these
docs won't pretend otherwise.

Want to check? Do a release build and run `python bench/bench.py`. The absolute
numbers will shift with your machine; the ratios are the point.

## A note on the FFI

Every foreign-function feature (calling DLLs, building structs, COM, callbacks)
lives deliberately on the *cold* path. None of it touches the hot numeric code
generation above. Adding the entire DirectX story cost the benchmarks nothing.
