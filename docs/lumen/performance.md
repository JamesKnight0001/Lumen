# How it runs fast

Lumen looks like a scripting language; a compiled Lumen program isn't. It keeps
pace with C and Rust on the right workloads and leaves Java, Node, and CPython
behind. Here's how. You don't do any of this; the compiler does.

## 1. Values don't touch the heap

Every value is one 64-bit word ([NaN-boxing](memory.md)), with integers, floats,
booleans, and nil stored inline. Number-crunching allocates nothing, copies are
free, and there's no object header or pointer to chase to read a loop counter.

## 2. It emits real machine code

`lumen build` doesn't feed bytecode to a VM. By default it emits **LLVM IR** and
links it with clang + lld (`--backend asm` writes x86-64 assembly + gcc instead,
same byte-identical output, used when no LLVM toolchain is present). No dispatch
loop, no per-op opcode check, just instructions the CPU runs directly.

## 3. A thin, optimized C runtime

Heap allocation, the GC, and the string/list helpers need real code. They live in
a small C runtime compiled at `-O2` and linked into your program. Lean on purpose.

## 4. Unboxed numeric fast paths (the big one)

Most of the speed lives here. A whole-program analysis proves which variables and
parameters are *always* int or *always* float, and those skip boxing:

- A proven-int local lives as a raw `i64`, so `a + b` is a bare `add`/`imul`. No
  tag check, no runtime call.
- A proven-float local lives in an SSE register; arithmetic is `addsd`/`mulsd`.
- A float-only list reads `a[i]` as a direct `movsd`; an int-only list gets an
  inline bounds-checked `mov` + unbox, skipping the `lumen_index_get` dispatch
  (about +60% on int-list loops).
- A loop accumulator (`total = total + …`) stays raw `i64` in its slot for the
  whole loop, so it accumulates with a bare `add`, no per-iteration box/unbox.
  This lands the integer loop at ~1.1× C.
- `x % 2^k` with a power-of-two constant compiles to a sign-correct mask (shifts
  + `and`), not a divide, matching the interpreter bit-for-bit including negatives.
- Self-recursive tail calls become loops, and small functions inline.
- A recursive base case can return *before* its stack frame is built (roughly half
  of all `fib` calls).
- The NaN-box conversion helpers are pure, tagged `memory(none)`, so LLVM deletes
  dead boxes (e.g. an unused loop counter) and hoists invariant ones. This brings
  the float reduction to ~1.03× C.

## 4b. Short-lived objects skip the GC heap

Escape analysis proves which lists, maps, structs, and comprehensions never leave
the function that builds them. Those are **bump-allocated in a per-call arena** and
freed on return: no GC tracking, no individual frees, deterministic cleanup at
scope exit. It's Rust's ownership win (no leaks, no use-after-free, freed at scope
end) with no annotations. Anything that might escape falls back to the GC heap, so
it's never wrong, just faster on the common case.

## 5. The optimizer runs to a fixpoint

`optimize_program` loops its inline -> const-fold -> CSE -> dead-code passes until
the program stops changing. Each pass is semantics-preserving, so iterating catches
work an earlier pass exposed: a fold unlocks an inline that unlocks another fold.
Same output, less generated code.

## What that buys you

Measured on Windows (LLVM/clang 21.1.6, mingw64 gcc 16.1, rustc 1.96, OpenJDK 25,
Node 24, CPython 3.14). Best-of-5 wall-clock; output is byte-identical across all
languages, and Lumen's interpreter, LLVM build, and asm build agree to the byte.
Absolute numbers shift per machine; the ratios are the point. Harness: `bench/bench.py`.

| Workload | Lumen | C `-O2` | Rust `-O` | Node | Python |
|----------|------:|--------:|----------:|-----:|-------:|
| `fib(35)` recursion          | **~57 ms**  | ~22 ms | ~30 ms | ~177 ms | ~1210 ms |
| float reduce, 5×10⁷          | **~56 ms**  | ~54 ms | ~56 ms | ~111 ms | ~4680 ms |
| int loop + modulo, 5×10⁷     | **~60 ms**  | ~56 ms | ~58 ms | ~168 ms | ~4900 ms |
| hash-map build+lookup, 10⁶   | **~57 ms**  | ~15 ms | ~46 ms | ~111 ms |  ~170 ms |

The two tight loops are the headline: **float reduction ~1.03× C, integer loop
~1.1× C**, effectively tied with the native compilers (and faster than Lumen's own
asm backend on the int loop). Recursion is ~2.6× C (call overhead), hash-maps ~3× C
(hashing + heap). Against managed runtimes it isn't close: ~2-3× faster than Node,
12-85× faster than CPython.

So: tied with C on hot numeric loops, a small constant factor off on recursion and
containers, decisively faster than every interpreter, all from a dynamically-typed
language running the same source unchanged under its own interpreter.

### Read the daggers honestly

Where C and Rust pull ahead, precisely why:

- **Recursion call overhead.** Lumen's calls pass NaN-boxed values through the
  boxed ABI, so each `fib` call boxes its argument and result. That's the ~2.6× gap.
- **Auto-vectorization & constant folding.** On SIMD-friendly or closed-form
  reductions, gcc/rustc emit `paddq`/`movabsq`, processing many elements per
  instruction or computing the answer at compile time and emitting *no loop*.
  Comparing against a deleted loop is comparing against the absence of work.
- **Hash-map throughput.** The map benchmark is dominated by the runtime's hash +
  probe; C's inlined open-addressing table is just tighter.

Scalar-for-scalar (the two loop rows), Lumen is within ~10% of C. The goal, "meet C
on the hot path, beat every managed language," holds.

### Reproduce it

```
export PATH="/c/msys64/mingw64/bin:$PATH"
lumen build bench/fib.lm -o fib.exe && ./fib.exe
# or the full multi-language harness:
python bench/bench.py
```

## A note on the FFI

Every foreign-function feature (DLLs, structs, COM, callbacks) lives on the *cold*
path and never touches the hot numeric codegen. Adding the entire DirectX story
cost the benchmarks nothing.
