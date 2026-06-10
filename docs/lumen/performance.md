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
  `movsd` from contiguous memory; a list proven to hold only **integers** gets
  an inline bounds-checked `mov` + unbox, skipping the `lumen_index_get` runtime
  dispatch entirely (this is the S3 work - about +60% on int-list-heavy loops).
- A reduction accumulator in a `for` loop (`total = total + …`) is hoisted into a
  callee-saved register for the whole loop instead of being written back to its
  stack slot every iteration (the S10 work - about +23% on accumulator loops).
- `x % 2^k` with a constant power-of-two divisor compiles to a sign-correct mask
  (a few shifts + an `and`) instead of a divide, matching the interpreter's
  remainder bit-for-bit including for negatives (the S11 work - about +19% on
  modulo-heavy loops like collatz).
- Self-recursive tail calls become plain loops, and small functions get inlined.
- A recursive function's base case can return *before* its stack frame is even
  built, which on something like `fib` is roughly half of all calls.

## 5. The optimizer runs to a fixpoint

`optimize_program` runs its inline → const-fold → CSE → dead-code passes in a
bounded loop until the program stops changing, instead of a single pass. Each
pass is semantics-preserving, so iterating only catches the work an earlier pass
exposed - a fold that unlocks an inline that unlocks another fold. Same output,
strictly less generated code (the S5 work).

## What that buys you

Measured on this machine (Windows, mingw64 gcc 15.2.0, rustc 1.96, Java 25,
Node 24, CPython 3.12). Medians of repeated runs with warmup discarded; every
program's output is byte-identical across all languages, because it's the same
program. Absolute numbers shift per machine - the ratios are the point.

### Fair scalar workloads (the honest comparison)

These are loops the C/Rust compilers **cannot** constant-fold or auto-vectorize
away, so it's genuinely Lumen's scalar code vs theirs:

| Workload | Lumen 0.77 | C `-O2` | Rust `-O` | Node | Python |
|----------|-----------|---------|-----------|------|--------|
| `fib(34)` recursion          | **~32 ms**  | ~15 ms | ~21 ms | ~117 ms | ~1132 ms |
| collatz (branchy, data-dependent) | **~139 ms** | ~52 ms | ~35 ms | ~282 ms | ~3220 ms |

On recursion Lumen is ~2× C and ahead of Rust-less-so but well ahead of Node and
Python. On collatz - a deliberately un-optimizable, branch-heavy scalar loop -
Lumen is ~2.7× C, ~4× Rust, **2× faster than Node, and ~23× faster than Python.**
That's the real shape of it: ~2-3× off the native compilers on honest scalar
code, decisively faster than every interpreter.

### Container workloads

| Workload | Lumen 0.77 | C `-O2` | Rust `-O` | Node | Python |
|----------|-----------|---------|-----------|------|--------|
| int-list sum, 100M reads | **~225 ms** | ~21 ms† | ~15 ms† | ~182 ms | ~8975 ms |

The int-list reductions are where S3 shows up: 0.75 ran this in ~549 ms, 0.77
runs it in ~225 ms - a **2.4× generational speedup**, now ahead of Node and ~40×
ahead of Python.

### Read the daggers honestly

The `*`/`†` cases are where C and Rust look untouchable, and it's worth being
precise about *why*:

- **Auto-vectorization.** On the int-list sum, gcc and rustc emit SIMD (`paddq`,
  processing multiple elements per instruction). Lumen emits honest scalar loads.
  Matching this needs a vectorizer, which Lumen does not have.
- **Whole-loop constant folding.** On a closed-form reduction like
  `sum(i for i in 0..1e8)`, gcc computes the answer at compile time and emits a
  single `movabsq` - the loop is *gone* from the binary. That "5 ms" isn't C
  running a fast loop; it's C running no loop. Comparing against it is comparing
  against the absence of work.

Neither is Lumen "losing at the same task" - they're the C/Rust compilers
deleting or transforming the task. Where the work actually has to happen (the
scalar table above), Lumen is within a small constant factor and the goal -
"meet C on the hot path, beat every managed language" - holds. These docs won't
pretend otherwise.

### Reproduce it

```
export PATH="/c/msys64/mingw64/bin:$PATH"
lumen build bench/fib.lm -o fib.exe && ./fib.exe
# or the full multi-language harness:
python bench/bench.py
```

## A note on the FFI

Every foreign-function feature (calling DLLs, building structs, COM, callbacks)
lives deliberately on the *cold* path. None of it touches the hot numeric code
generation above. Adding the entire DirectX story cost the benchmarks nothing.
