# Variables

`let` introduces a new name. Plain assignment updates one that already exists.
Two jobs, two pieces of syntax.

```lumen
let x = 10        # bind a new name
x = x + 1         # reassign the existing name
```

The split is deliberate. `let` says "I'm making something new"; bare `=` says
"I'm changing something that's already here." That distinction keeps typos
honest: assigning to a name you never `let` is an error, not a silently created
global that bites you three functions later.

## Compound assignment

The usual shorthands are all here, and they mean exactly what they look like:

```lumen
let total = 0
total += 5        # total = total + 5
total -= 2        # total = total - 2
total *= 3        # total = total * 3
total /= 2        # total = total / 2
```

`x OP= e` is just sugar for `x = x OP e`, nothing more.

## Scope

Names live in the block that introduced them, plus any blocks nested inside that
one. Function parameters are local to the function. The loop variable in a `for`
or a [list comprehension](lists.md#list-comprehensions) is scoped to that loop and
doesn't leak out once the loop ends.

```lumen
fn main():
    let n = 3
    for i in 0..n:
        let sq = i * i
        print(sq)
    # i and sq are not visible here
```

## A note for the curious

Whether a variable ends up as a fast unboxed machine integer or float, or as a
boxed value, is decided by the compiler's analysis, not by you. You just write
`let` and move on. If you're curious what's happening underneath, see
[how it runs fast](../lumen/performance.md). It never changes behavior, only
speed.
