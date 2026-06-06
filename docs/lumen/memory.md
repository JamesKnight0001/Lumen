# How memory works

You don't manage memory in Lumen. No `malloc`, no `free`, no ownership
annotations, no `drop` you're obligated to call. You make values, and they go away
when nothing can reach them anymore. That's the whole story from where you sit.
This page is about what happens underneath, because every now and then it shows
through.

## Values are cheap; only some of them live on the heap

Every value is a single 64-bit word, packed with a trick called **NaN-boxing**.
Integers, floats, booleans, and `nil` live entirely inside that word. They never
touch the heap and cost nothing to copy. The only things that get a real heap
allocation are the ones that genuinely need variable-sized storage: **strings,
lists, maps, structs, and closures**. The word just points at them.

The upshot is nice. A loop that sums a million integers allocates nothing at all.
A loop that builds a million-element list allocates the list, and not much else.

Packing everything into one word costs you something, though: integers get a
**48-bit payload**, not the full 64. They're exact out to roughly ±1.4×10¹⁴, and
arithmetic past that **wraps around** instead of crashing (identically on both
backends, so it's at least predictable). Need bigger numbers? Reach for floats.
[values](../syntax/values.md#integers-are-48-bit) has the exact range.

## How unreachable memory is reclaimed

The two backends do this differently under the hood. The effect is the same, and
you never think about either one:

- **The interpreter** leans on Rust's reference counting (`Rc`). The moment the
  last reference to a value drops, the value does too.
- **The compiled binary** carries a small **generational mark-and-sweep garbage
  collector** in its C runtime. Every so often it pauses, scans the stack
  conservatively to see what's still reachable, and frees the rest.
  "Generational" just means it looks at young objects more often than old
  survivors, which is cheap and happens to match how most programs actually
  behave.

Either way, the rule is the same: once a heap object is unreachable, it's gone,
and you didn't have to lift a finger.

## Seeing what the GC did

Curious how much your program churned through? Run a compiled binary with
`LUMEN_GC=1` and it prints an allocation report on exit.

```
LUMEN_GC=1 ./myapp.exe
```

## `drop`, an optional nudge you never have to use

There's a `drop(x)` builtin that says "I'm done with this big value, feel free to
reclaim it now." It's purely an optimization, for the rare moment when you're
holding something large and want it gone before the GC would get around to it. In
the interpreter it does nothing at all. And forgetting it can't leak or corrupt
anything; the worst case is the GC reclaims on its own schedule instead of yours.

## The short version

Make values, use them, let them go. The language cleans up after you. The one
memory fact that ever reaches everyday code is that 48-bit integer ceiling, and
only if you deliberately shove past ±140 trillion.
