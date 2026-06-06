# Lumen SSA mid-IR + linear-scan register allocator - implementation plan

> **For Hermes:** build incrementally; every task ends byte-identical
> (`python tests/run_tests.py` = 32/32) + clippy 0 + cargo test. Commit per task
> with measured numbers. REVERT any task with no measured gain. This is the
> Phase 4.1 lever from todo.md; combine with continued targeted wins (item 2).

**Goal:** close the remaining gap to C on call-heavy + spill-heavy code (fib 2.8×,
int loop 1.8×) by introducing a small per-function mid-IR for proven-numeric
functions and a linear-scan register allocator that keeps loop-carried and
call-surviving values in callee-saved registers instead of stack slots.

**Architecture (risk-confined):** the existing 3400-line single-pass emitter in
codegen.rs STAYS as the default and fallback. A NEW module `mir.rs` builds a
three-address SSA-ish IR for a function ONLY when the function qualifies (see
gate). Codegen calls the MIR path for qualifying functions and the legacy path
for everything else. Unchanged functions emit byte-identical asm by construction;
MIR-path functions are proven byte-identical against the interpreter via the
differential suite + adversarial programs. If a MIR function ever can't be
lowered soundly, it falls back to the legacy emitter (never wrong, just slower).

**Tech stack:** Rust, x86-64 (Win64 ABI), GNU as/ld via the existing build path.

## The MIR qualification gate (start MAXIMALLY conservative)

A function qualifies for the MIR path only if ALL hold (else legacy emitter):
- it is a free function (not a method/closure target),
- every parameter and the return are proven int OR proven float (types.rs lattice),
- its body uses ONLY: let/assign, if/elif/else, while, for-range, return,
  arithmetic/comparison on proven-numeric values, and direct calls to other
  proven-numeric free functions (the raw calling convention),
- NO lists/maps/structs/strings/closures/first-class fns/indexing/slicing/
  comprehensions/fstrings/method calls/print in the body (these stay legacy).
- fib and the numeric microbenchmarks qualify; the AI/stocksim/textquest do not.

This gate makes the FIRST landing safe and small. Widen it later, one construct
at a time, each widening re-verified byte-identical.

---

## Task 1: scaffold mir.rs + the qualification gate (no behavior change)

**Files:** Create `compiler/src/mir.rs`; modify `compiler/src/lib.rs` (add `pub mod mir;`),
`compiler/src/codegen.rs` (call the gate, but for now ALWAYS fall back to legacy).

- Define `fn mir_eligible(f: &FnDef, info: &IntInfo) -> bool` implementing the gate.
- In codegen's per-function emit, compute `mir_eligible`; if true, log nothing and
  still emit via the legacy path (the MIR lowering lands in Task 4). This task only
  adds the gate + module skeleton.
- **Verify:** builds, clippy 0, suite 32/32 (no behavior change - pure scaffolding).
- **Commit:** "mir: scaffold module + conservative eligibility gate (no codegen change)".

## Task 2: define the MIR data types

**Files:** `compiler/src/mir.rs`.

- `enum Val { IntConst(i64), FloatConst(u64), Vreg(u32), Param(u32) }`
- `enum Op { Add, Sub, Mul, DivConst(i64), ModConst(i64), Idiv, Imod, FAdd, FSub,
  FMul, FDiv, Lt, Le, Gt, Ge, Eq, Ne, FLt.. , Neg, FNeg }`
- `enum Inst { Bin{dst:Vreg, op, a:Val, b:Val}, Move{dst,src}, Call{dst, fn, args:Vec<Val>},
  Ret(Option<Val>), Br{cond:Val, t:Block, f:Block}, Jmp(Block), Phi{dst, srcs:Vec<(Block,Val)>} }`
- `struct MirFn { params, blocks: Vec<Block>, is_float: bool }`, `struct Block { id, insts }`
- Tests: construct a tiny MirFn by hand, assert Debug shape. **Verify/commit.**

## Task 3: lower a qualifying AST fn -> MIR (with SSA construction)

**Files:** `compiler/src/mir.rs`.

- Build basic blocks from the structured AST (if/while/for-range -> CFG with
  blocks + branches). Because Lumen has structured control flow only (no goto),
  SSA construction is the simple "sealed blocks" case (Braun et al. 2013):
  per-variable current-definition map, phi at block joins for vars live across.
- Loop-carried vars (the counter, accumulators) become phis at the loop header.
- Keep 48-bit wrap explicit: every Add/Sub/Mul that could overflow gets a
  wrap-to-48 marker so the emitter masks identically to the interpreter.
- Unit tests: lower `fn add(a,b)=a+b`, `fn fib(n)`, a for-range sum; assert block
  count + phi placement. **Verify/commit.**

## Task 4: emit x86-64 from MIR with a STACK allocator first (byte-identical bring-up)

### INTEGRATION CONTRACTS (extracted from codegen.rs - verified this session)
The MIR emitter must produce byte-identical *behavior* (not text) vs the legacy
path. Reuse these exact primitives (all on `Codegen`/`Self` in codegen.rs):
- **Dispatch point:** `gen_fn_impl` (codegen.rs ~L644) already has the inert gate:
  `const MIR_ENABLED: bool = false; if MIR_ENABLED && !raw_abi && mir_eligible(...)`.
  Task 4 flips this: when eligible, build `mir::lower_fn(f, &sigs)` then emit and
  RETURN that function's asm string in place of the legacy body. Build a
  `mir::SigMap::from_program(prog)` ONCE in `generate()` and stash on self.
- **Frame/ABI:** the eligible fn is emitted as BOTH `lm_<name>` (boxed entry) and
  `lm_<name>.raw` (raw entry) - same as today. EASIEST bring-up: only route the
  `.raw` entry (raw_abi=true) through MIR; keep boxed `lm_<name>` legacy. Raw
  entry: params arrive raw i64 in rcx/rdx/r8/r9 (5th+ at [rbp+48+...]); return
  raw i64 in rax. Float fns: see float path below.
- **Box/unbox immediates (no calls):** int box = `0x7FF9<<48 | (n & 0xFFFFFFFFFFFF)`;
  `emit_box_int`/`emit_unbox_int` (shl16/sar16). bool box header 0x7FFA, nil 0x7FFB.
  float const: `add_double(bits)` -> `.fconstN: .quad bits`, load `movsd xmm,[rip+lbl]`.
- **Div/mod by zero TRAP (must match exactly):** guard `test rax,rax; jne nz`;
  zero path boxes both operands and `call lumen_div`/`lumen_mod` (Self::runtime_op_name)
  which never returns (emits the exact die message+line+exit). nz path: `mov rcx,divisor;
  mov rax,dividend; cqo; idiv rcx;` quotient=rax, remainder=rdx (mov rax,rdx for mod).
  48-bit values are never INT64_MIN so no #DE on MIN/-1.
- **Magic division by const:** reuse the existing DivConst/ModConst legacy helper
  (Granlund-Montgomery) - find `emit_div_by_const`-style code near L1944/L2095.
- **Calls:** to another eligible fn use the `.raw` entry exactly like `emit_raw_call`
  (codegen.rs ~L3085): args -> spill slots -> rcx/rdx/r8/r9 (+stack for 5th), 32B
  shadow space, 16B alignment, result raw in rax.
- **lumen_set_line:** legacy emits a set-line call per SrcLine for runtime error
  reporting. MIR drops SrcLine; the ONLY runtime error in eligible fns is div0,
  whose line must still match - so emit a `lumen_set_line` before each potentially-
  trapping div/mod (carry the line via a wrap of SrcLine into the MIR, OR keep the
  most-recent line in the lowerer and attach to IDiv/IMod insts). DECIDE before coding.
- **Stack allocation:** every vreg -> a distinct rbp-relative slot (bump ctx.stack_size
  / `temp(ctx)`). Phis: emit as moves on the incoming edges (phi elimination:
  before each Jmp/Br to a block with phis, store the edge's value into the phi's
  slot). No regalloc yet (Task 5).
- **Verify:** THE GATE - full differential suite 32/32 byte-identical. fib + numeric
  examples now flow through MIR's .raw entry. Add adversarial numeric programs
  (mutual recursion, deep recursion, every operator, negatives, 48-bit wrap, div0
  message+line+exit). Perf NEUTRAL-or-worse OK. Commit only if byte-identical; else
  REVERT to MIR_ENABLED=false.



## Task 5: linear-scan register allocation over the MIR

**Files:** `compiler/src/mir.rs`.

- Compute live intervals over a linearization of the blocks. Allocate the
  Win64-volatile regs (rax,rcx,rdx,r8-r11) for short-lived temps and callee-saved
  (rbx,r12-r15 / xmm6-15) for values live across a call or loop back-edge. Spill
  to stack slots when registers run out (the existing behavior as the floor).
- Honor the Win64 ABI at calls: caller-saved regs holding live values are spilled
  around calls (or the value lives in a callee-saved reg). Save/restore callee-
  saved regs in prologue/epilogue (reuse the v0.49/v0.50 save/restore machinery).
- **THE GATE:** suite 32/32 byte-identical again. Then BENCH: fib + int loop.
  Expected: fib's n + recursive results stay in registers -> the prologue/spill
  traffic drops. Target fib 2.8x -> ~1.8-2.0x, int loop 1.8x -> ~1.4-1.6x.
- **Commit with measured numbers.** If NO measured gain over legacy, REVERT the
  whole MIR path (keep it behind the gate disabled) and record why in todo.md -
  same discipline as the reverted int-accumulator.

## Task 6+ (widen, only if Task 5 paid off)

Each widening is its own task, re-verified byte-identical + measured:
- 6a: allow proven-float fns through MIR (mirror int).
- 6b: LICM over the MIR (hoist loop-invariant `x*c` etc.) - now SOUND because the
  MIR has explicit blocks + dominance (the AST-level blocker was no block scoping).
- 6c: GVN / common-subexpression on the MIR.
- 6d: widen the gate toward list/struct ops (much later; high risk).

## Continued targeted wins (item 2, interleaved)
Land any independent, measured, byte-identical peephole as it's found (e.g. fold
`x*2`->`add`, `lea` for `a+b*k` address-ish int math) - but only with a measured
gain; the easy ALU seam is mostly mined out (v0.51 showed compare-branch + imm
folding give tighter code but no headline move because fib is call-bound).

## Discipline (every task)
cargo build (debug+release) · python tests/run_tests.py = 32/32 byte-identical ·
cargo clippy = 0 · cargo test · adversarial numeric programs byte-identical ·
`lumen emit` confirms the MIR path fired · bench before/after · commit per task ·
REVERT anything with no measured gain (correct-but-slow backout = success).
