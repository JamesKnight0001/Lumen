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
runtime. By default it emits **LLVM IR** and links it with clang + lld into a
native executable (`--backend asm` instead writes x86-64 assembly and links with
gcc - same byte-identical output, used as the fallback when no LLVM toolchain is
present). Either way: no dispatch loop, no "which opcode is this" check on every
operation. Just instructions the CPU runs directly.

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
- A reduction accumulator in a `for`/`while` loop (`total = total + …`) stays
  unboxed for the whole loop - on the LLVM backend a proven-int local lives as a
  raw `i64` in its stack slot (which LLVM promotes to a register), so the loop
  accumulates with a bare `add` and no per-iteration box/unbox. The integer-loop
  benchmark lands at ~1.1× C this way.
- `x % 2^k` with a constant power-of-two divisor compiles to a sign-correct mask
  (a few shifts + an `and`) instead of a divide, matching the interpreter's
  remainder bit-for-bit including for negatives.
- Self-recursive tail calls become plain loops, and small functions get inlined.
- A recursive function's base case can return *before* its stack frame is even
  built, which on something like `fib` is roughly half of all calls.
- The NaN-box conversion helpers (`lumen_from_int`, `lumen_to_int`, and the float
  pair) are pure, so they're tagged `memory(none)`: LLVM then deletes dead boxes
  (e.g. a loop counter the body never reads) and hoists loop-invariant ones. This
  is what brings the float reduction to ~1.03× C.

## 4b. Short-lived objects skip the GC heap

A second whole-program analysis (escape analysis) proves which lists, maps,
structs, and comprehensions never escape the function that builds them. Those are
**bump-allocated in a per-call arena** and freed wholesale when the function
returns - no GC tracking, no individual frees, deterministic cleanup at scope
exit. It's the memory-safety win of Rust's ownership (no leaks, no
use-after-free, freed when the scope ends) without any annotations: you write
plain Lumen and the compiler proves the lifetime. Objects that *might* escape
fall back to the GC heap automatically, so it's never wrong - just faster when it
can prove the common case.

## 5. The optimizer runs to a fixpoint

`optimize_program` runs its inline → const-fold → CSE → dead-code passes in a
bounded loop until the program stops changing, instead of a single pass. Each
pass is semantics-preserving, so iterating only catches the work an earlier pass
exposed - a fold that unlocks an inline that unlocks another fold. Same output,
strictly less generated code (the S5 work).

## What that buys you

Measured on this machine (Windows; LLVM/clang 21.1.6, mingw64 gcc 16.1,
rustc 1.96, OpenJDK 25, Node 24, CPython 3.14). Best-of-5 wall-clock; every
program's output is byte-identical across all languages, because it's the same
program, and Lumen's interpreter, LLVM build, and asm build agree to the byte.
Absolute numbers shift per machine - the ratios are the point. The harness is
`bench/bench.py`.

| Workload | Lumen | C `-O2` | Rust `-O` | Node | Python |
|----------|------:|--------:|----------:|-----:|-------:|
| `fib(35)` recursion          | **~57 ms**  | ~22 ms | ~30 ms | ~177 ms | ~1210 ms |
| float reduce, 5×10⁷          | **~56 ms**  | ~54 ms | ~56 ms | ~111 ms | ~4680 ms |
| int loop + modulo, 5×10⁷     | **~60 ms**  | ~56 ms | ~58 ms | ~168 ms | ~4900 ms |
| hash-map build+lookup, 10⁶   | **~57 ms**  | ~15 ms | ~46 ms | ~111 ms |  ~170 ms |

The two tight numeric loops are the headline: the **float reduction is ~1.03× C
and the integer loop ~1.1× C** - effectively tied with the native compilers, and
faster than Lumen's own asm backend on the int loop. Recursion sits at ~2.6× C
(call overhead) and hash-map throughput at ~3× C (hashing + heap). Against the
managed runtimes it isn't close: Lumen is ~2-3× faster than Node and 12-85×
faster than CPython across the board.

That's the real shape of it: tied with C on hot numeric loops, a small constant
factor off on recursion and containers, and decisively faster than every
interpreter - from a dynamically-typed language running the same source
unchanged under its own interpreter.

### Read the daggers honestly

Where C and Rust still pull ahead, it's worth being precise about *why*:

- **Recursion call overhead.** On `fib`, C/Rust have near-zero per-call cost;
  Lumen's calls still pass NaN-boxed values through the boxed ABI, so each
  recursive call boxes its argument and result. That's the ~2.6× gap.
- **Auto-vectorization & constant folding.** On closed-form or SIMD-friendly
  reductions, gcc and rustc emit `paddq`/`movabsq` - processing many elements per
  instruction, or computing the answer at compile time and emitting *no loop at
  all*. Lumen emits honest scalar code. Comparing against a deleted loop is
  comparing against the absence of work.
- **Hash-map throughput.** The map benchmark is dominated by the C runtime's hash
  + probe; C's inlined open-addressing table is simply tighter.

Where the work actually has to happen scalar-for-scalar (the two loop rows),
Lumen is within ~10% of C. The goal - "meet C on the hot path, beat every managed
language" - holds, and these docs won't pretend otherwise.

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
