# Values and types

Lumen is dynamically typed: type belongs to the value, not the variable. There
are seven kinds of value.

| Kind | Looks like | `type(x)` gives |
|------|-----------|-----------------|
| integer | `0`, `42`, `-7` | `"i64"` |
| float | `3.14`, `1.0`, `2e3` | `"f64"` |
| bool | `true`, `false` | `"bool"` |
| string | `"hi"`, `f"x = {x}"` | `"str"` |
| list | `[1, 2, 3]`, `[]` | `"list"` |
| map | `{"a": 1}`, `{}` | `"map"` |
| struct | `Point(x: 1, y: 2)` | `"struct"` |
| nil | `nil` | `"nil"` |

`type(v)` returns the type name as a string. Handy for dispatch and debugging.

## Numbers: integers vs floats

Integer arithmetic stays integer, and `/` is integer division:

```lumen
print(7 / 2)        # 3      (integer division - the remainder is dropped)
print(7 % 2)        # 1      (modulo)
print(7.0 / 2.0)    # 3.5    (any float operand makes the whole thing float)
print(2 + 3.0)      # 5.0    (mixing promotes to float)
```

The arithmetic operators are `+ - * / %` and `**` (exponent). Two things about
`**` trip people up: it's right-associative, and it binds tighter than `* / %`:

```lumen
print(2 ** 3 ** 2)  # 512    (right-associative: 2 ** (3 ** 2))
print(-2 ** 2)      # -4     (** binds tighter than the unary minus)
print(2 ** 10)      # 1024   (int ** non-negative int stays an integer)
print(2 ** -1)      # 0.5    (negative exponent, or any float, gives a float)
```

### Integers are 48-bit

Because every value packs into one 64-bit word (see
[how memory works](../lumen/memory.md)), integers use a 48-bit two's-complement
payload: the range `[-140737488355328, 140737488355327]`, i.e. `[-2⁴⁷, 2⁴⁷−1]`.
Overflowing that range **wraps around** rather than trapping, and it wraps
identically in the interpreter and the compiled binary (so `max + 1 == min` in
both). Within range, integers are exact. Need bigger magnitudes? Reach for
floats.

## Booleans and nil

`true` and `false` are the booleans. `nil` is the "no value" value: what
functions return when they don't `return` anything, and what a failing
standard-library lookup gives you.

Lumen has **strict truthiness**. Anywhere a condition is expected (`if`, `while`,
the ternary, `and`/`or` operands), the value must be an actual bool. `if 5:` is
an error, not a shortcut for "5 is truthy." Write the comparison you mean:
`if n != 0:`, `if name != nil:`. A small cost up front that rules out a whole
class of bugs.

Combine booleans with `and`, `or`, and `not`. They require bool operands too, per
the same rule:

```lumen
print(true and false)   # false
print(true or false)    # true
print(not true)         # false
if 0 <= i and i < len(xs):
    print(xs[i])
```

## Equality and comparison

`==` and `!=` test value equality across int and float. The ordering operators
`< <= > >=` work on numbers and strings. Strings compare lexicographically, byte
by byte, so `"apple" < "banana"`, and uppercase sorts before lowercase:

```lumen
print(1 == 1.0)         # true
print("Dog" < "dog")    # true   (uppercase 'D' sorts before lowercase 'd')
```

## Membership: `in` / `not in`

`x in container` returns a bool and means the natural thing for each type:

```lumen
print(3 in [1, 2, 3])     # true   (element of a list, by value)
print("ell" in "hello")   # true   (substring of a string)
print("k" in {"k": 1})    # true   (key of a map)
print(9 not in [1, 2, 3]) # true
```

## Trailing commas are fine

Lists, maps, call arguments, and parameter lists all allow a trailing comma. It
keeps your diffs clean:

```lumen
let xs = [
    1,
    2,
    3,
]
```
