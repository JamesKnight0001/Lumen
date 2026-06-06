# Lumen "as fast as possible" - performance plan

> Strategy: a sequence of targeted, individually byte-identical + measured codegen
> wins, NOT one giant SSA rewrite. Each lands only if it preserves the
> interp==native contract (suite green) AND measurably helps (or is reverted).
> The SSA mid-IR remains the eventual big lever but is deferred until the cheap,
> safe wins are exhausted - it is a multi-week rewrite that risks the contract
> everywhere.

Baseline (v0.50, best-of-5 via `python bench/bench.py`, gcc -O2 ref):
- fib(35): 65ms = 2.8x of C   <- WORST gap, dominated by call overhead + per-call
  boxed-bool round-trip + stack spills of n/intermediates
- float reduce 5e7: 51ms = 1.0x of C  (DONE - at parity)
- int loop 5e7: 91ms = 1.8x of C  (division-bound)

## Task 1: Fused proven-int compare-and-branch (kills the boxed-bool round-trip)

**Observation (from `lumen emit` on fib):** an `if n < 2:` condition emits the full
boxed comparison `cmp r8,r9; setl al; movzx rax,al; or rax,0x7FFA<box>; test al,1; jz`
then branches. The box is dead - the branch only needs the flags. 7 instrs -> 2.

**Fix:** at the top of `gen_cond_false`, if `cond` is `Binary{op in <,<=,>,>=,==,!=,
lhs, rhs}` with BOTH operands `expr_known_int`, load r8=lhs r9=rhs (reuse the
simple_raw_operand / eval_raw logic from the Binary cmp fast path), emit
`cmp r8, r9` then a single `j<inverted-cc> target` (jump when cond is FALSE).
Inverted map: < -> jge, <= -> jg, > -> jle, >= -> jl, == -> jne, != -> je.

**Soundness:** identical observable behavior - proven-int operands can't error, and
the branch outcome is exactly what the boxed `test al,1` would have produced.
Hits every if/while/elif with an int comparison condition (n<2, i<n, ...).

**Verify:** full suite 32/32 byte-identical, clippy 0, cargo test; adversarial
if/elif/while with int comparisons incl negative operands and ==/!=; emit shows
`cmp;jcc` with no `0x7FFA` box in the condition. Bench fib.

## Task 2: `sub rax, <imm>` / `add rax, <imm>` for literal operands (eval_raw)

`n - 1` currently emits `mov rcx,1; sub rax,rcx`. x86 takes an immediate directly:
`sub rax, 1`. Small but in every fib call (n-1, n-2). Low risk.

## Task 3 (bigger): keep fib's `n` + call results in callee-saved regs

fib spills n to [rbp-8] and reloads 3x, spills each recursive result. A
mid-IR-free targeted allocator for leaf proven-int fns could keep n in rbx and
results in r14/r15. Higher risk; measure Task 1+2 first - they may already shrink
the gap materially.

## Deferred: SSA mid-IR (Phase 4.1)

The general answer (GVN, LICM, full linear-scan regalloc). ~2000 LOC parallel
subsystem + codegen rewire. Defer until 1-3 are done and measured; revisit if the
remaining gap justifies the risk. Each SSA sub-pass must still be proven
byte-identical against the interpreter.

## Discipline (every task)
1. cargo build (debug+release), 2. python tests/run_tests.py = 32/32,
3. cargo clippy = 0, cargo test green, 4. adversarial programs byte-identical,
5. `lumen emit` confirms the asm changed as intended, 6. bench before/after,
7. commit per task with measured numbers; REVERT anything with no measured gain.
