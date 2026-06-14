# How memory works

You don't manage memory in Lumen. No `malloc`, no `free`, no ownership
annotations, no `drop` you're obligated to call. You make values, and they go away
when nothing can reach them. That's the whole story from where you sit. This page
covers what happens underneath, where it occasionally shows through.

## Values are cheap; only some of them live on the heap

Every value is a single 64-bit word, packed with a trick called **NaN-boxing**.
Integers, floats, booleans, and `nil` live entirely inside that word. They never
touch the heap and cost nothing to copy. Only values that need variable-sized
storage get a real heap allocation: **strings, lists, maps, structs, and
closures**. The word just points at them.

The upshot: a loop summing a million integers allocates nothing. A loop building
a million-element list allocates the list, and not much else.

Packing everything into one word has a cost: integers get a **48-bit payload**,
not the full 64. They're exact out to roughly ±1.4×10¹⁴, and arithmetic past that
**wraps around** instead of crashing (identically on both backends, so it's at
least predictable). Need bigger numbers? Use floats.
[values](../syntax/values.md#integers-are-48-bit) has the exact range.

## How unreachable memory is reclaimed

The two backends do this differently. The effect is the same, and you never
think about either one:

- **The interpreter** leans on Rust's reference counting (`Rc`). The moment the
  last reference to a value drops, the value does too.
- **The compiled binary** carries a small **generational mark-and-sweep garbage
  collector** in its C runtime. Every so often it pauses, scans the stack
  conservatively for what's still reachable, and frees the rest. "Generational"
  means it checks young objects more often than old survivors, which is cheap and
  matches how most programs behave.

Either way: once a heap object is unreachable, it's gone, and you didn't lift a
finger.

## Seeing what the GC did

Curious how much your program churned through? Run a compiled binary with
`LUMEN_GC=1` and it prints an allocation report on exit.

```
LUMEN_GC=1 ./myapp.exe
```

## `drop`, an optional nudge you never have to use

There's a `drop(x)` builtin that says "I'm done with this big value, reclaim it
now." It's purely an optimization, for the rare moment when you're holding
something large and want it gone before the GC would. In the interpreter it does
nothing. Forgetting it can't leak or corrupt anything; worst case, the GC
reclaims on its own schedule instead of yours.

## The short version

Make values, use them, let them go. The language cleans up after you. The one
memory fact that reaches everyday code is the 48-bit integer ceiling, and only if
you deliberately shove past ±140 trillion.
