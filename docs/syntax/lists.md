# Lists

A list is an ordered, growable sequence. The elements can be any type, and they don't have to agree with each other, so mixing numbers and strings in one list is perfectly fine.

```lumen
let xs = [3, 1, 4]
print(xs[0])          # 3      (index access)
print(xs.len())       # 3
print([1, "two", 3.0]) # mixed types are fine
```

## Growing and shrinking (these mutate the list)

```lumen
let xs = [3, 1, 4]
xs.push(1)            # [3, 1, 4, 1]
let last = xs.pop()   # last == 1; xs is back to [3, 1, 4]
xs.insert(0, 99)      # [99, 3, 1, 4]
xs.sort()             # ascending numeric sort, in place
xs.reverse()          # reverse, in place
```

## Asking questions

```lumen
let xs = [10, 20, 30, 20]
print(xs.len())          # 4
print(xs.contains(20))   # true
print(xs.index(20))      # 1     (first match, or -1)
print(xs.count(20))      # 2
print(sum(xs))           # 80
print(min(xs))           # 10
print(max(xs))           # 30
print(["a","b"].join("-")) # "a-b"  (string elements only)
```

## Transforming with functions

`map` and `filter` each take a [function value](functions.md) and hand you back a fresh list. The original is never touched, which is exactly what you want when you're chaining transforms together:

```lumen
fn double(x): return x * 2
fn is_even(x): return x % 2 == 0

print([1, 2, 3].map(double))         # [2, 4, 6]
print([1, 2, 3, 4].filter(is_even))  # [2, 4]

# with an anonymous function (bind it to a name first):
let sq = fn(x): x * x
print([1, 2, 3].map(sq))             # [1, 4, 9]
```

## Slicing

`xs[lo:hi]` returns a half-open slice: it includes `lo` and excludes `hi`. The nice part is that slicing never blows up on you. Bounds clamp to the length, negative indices count back from the end, and either bound can be left off entirely.

```lumen
let xs = [10, 20, 30, 40, 50]
print(xs[1:3])     # [20, 30]
print(xs[-2:])     # [40, 50]   (negative start)
print(xs[2:100])   # [30, 40, 50]  (clamped)
print(xs[:2])      # [10, 20]   (open start)
print(xs[:])       # a full copy
```

## List comprehensions

Build a whole list from any iterable in a single expression:

```lumen
[element for var in iterable]
[element for var in iterable if condition]
```

The iterable can be a range, a list, or a string. The element and the condition can each be any expression you like, a [ternary](control-flow.md#the-ternary-expression) included. And don't worry about the loop variable: it's scoped to the comprehension and never leaks out into the surrounding code.

```lumen
print([x * x for x in 0..6])              # [0, 1, 4, 9, 16, 25]
print([n for n in 0..10 if n % 2 == 0])   # [0, 2, 4, 6, 8]
print([c for c in "lumen"])               # ["l", "u", "m", "e", "n"]
print(["even" if n % 2 == 0 else "odd" for n in 0..3])
```

Want the complete roster of methods? It's all on
[the standard library page](stdlib.md#list-methods).
