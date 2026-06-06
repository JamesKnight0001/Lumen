# Control flow

## `if` / `elif` / `else`

```lumen
if score >= 90:
    print("A")
elif score >= 80:
    print("B")
else:
    print("C")
```

`elif` and `else` are both optional. The condition has to be a **bool**: Lumen
has strict truthiness, so `if 5:` is an error, not a stand-in for "5 is truthy."
Write the comparison you actually mean (`if count != 0:`, `if name != nil:`). It
feels picky for about a day, then it quietly catches a whole class of bugs that
truthy/falsy languages happily let slip through.

## `while`

```lumen
let i = 0
while i < 5:
    print(i)
    i += 1
```

## `for`

`for` walks anything iterable: a range, a list, a string, or a map.

```lumen
# a half-open range: lo..hi includes lo, excludes hi
for i in 0..10:
    print(i)              # 0 through 9

# a list
for name in ["ana", "bo", "cy"]:
    print(name)

# a string yields its characters (as 1-char strings)
for ch in "hi":
    print(ch)             # "h", then "i"

# a map yields its keys
for key in {"a": 1, "b": 2}:
    print(key)            # "a", then "b"
```

`range(n)` is the same as `0..n`, and `range(lo, hi)` the same as `lo..hi`. Both
forms also produce a real list you can store and reuse.

## `break` and `continue`

Both work in `while` and `for`. `break` leaves the loop entirely; `continue`
skips to the next iteration.

```lumen
for n in 0..100:
    if n % 2 == 1:
        continue          # skip odd numbers
    if n > 10:
        break             # stop once we pass 10
    print(n)              # 0 2 4 6 8 10
```

## The ternary expression

`value if cond else other` is an *expression*: it evaluates to one of two values,
and only the chosen branch ever runs. It's the right tool for small inline
decisions:

```lumen
let label = "even" if n % 2 == 0 else "odd"
```

It chains right-to-left, and the result reads like a little decision table:

```lumen
let grade = "A" if score >= 90 else "B" if score >= 80 else "C"
```

For building lists conditionally, reach for
[list comprehensions](lists.md#list-comprehensions). For bailing out on errors,
see [errors](errors.md).
