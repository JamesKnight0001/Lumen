# Variables

`let` introduces a new name. Plain assignment updates one that already exists.
Two jobs, two pieces of syntax.

```lumen
let x = 10        # bind a new name
x = x + 1         # reassign the existing name
```

The split is deliberate. `let` says "I'm making something new"; bare `=` says
"I'm changing something that's already here." That keeps typos honest: assigning
to a name you never `let` is an error, not a silently created global.

## Compound assignment

The usual shorthands are all here, and they mean what they look like:

```lumen
let total = 0
total += 5        # total = total + 5
total -= 2        # total = total - 2
total *= 3        # total = total * 3
total /= 2        # total = total / 2
```

`x OP= e` is just sugar for `x = x OP e`.

## Destructuring

`let` can bind several names at once from a list, unpacking it element by
element. This is handy for functions that return multiple values:

```lumen
fn minmax(xs):
    return [xs[0], xs[-1]]

let lo, hi = minmax([3, 9])   # lo = 3, hi = 9
mut a, b = [1, 2]             # mut makes both reassignable
```

`for` destructures each item the same way, which makes pair iteration clean:

```lumen
let points = [[0, 0], [3, 4]]
for x, y in points:
    print(x + y)              # 0, then 7
```

The names must match the number of elements; unpacking more names than the
list holds is an out-of-range error. Destructuring is sugar: it lowers to a
temp plus one indexed bind per name, so it behaves identically everywhere.

## Scope

Names live in the block that introduced them, plus any nested blocks. Function
parameters are local to the function. The loop variable in a `for` or a
[list comprehension](lists.md#list-comprehensions) is scoped to that loop and
doesn't leak out once it ends.

```lumen
fn main():
    let n = 3
    for i in 0..n:
        let sq = i * i
        print(sq)
    # i and sq are not visible here
```

## A note for the curious

Whether a variable becomes a fast unboxed machine integer or float, or a boxed
value, the compiler decides, not you. Just write `let`. Curious what's
underneath? See [how it runs fast](../lumen/performance.md). It never changes
behavior, only speed.
