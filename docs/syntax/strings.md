# Strings

A string is text in double quotes. Indexing into one gives you a one-character
string back; there's no separate character type to worry about.

```lumen
let s = "hello"
print(s.len())        # 5
print(s[0])           # "h"   (a 1-character string)
print(s[1:4])         # "ell" (a slice - see below)
```

## f-strings: interpolation

Prefix a string with `f` and drop any expression inside `{...}`:

```lumen
let name = "World"
let a = 3
let b = 4
print(f"Hello, {name}!")        # Hello, World!
print(f"{a} + {b} = {a + b}")   # 3 + 4 = 7
```

## Building and combining

```lumen
print("ab" + "cd")              # abcd          (concatenation)
print("ab".repeat(3))           # ababab
print(",".join(["a", "b", "c"])) # a,b,c        (glue a list with this string)
let parts = "a,b,c".split(",")   # ["a", "b", "c"]
```

## Inspecting and transforming

```lumen
print("Hello".upper())              # HELLO
print("Hello".lower())              # hello
print("hi there".title())          # Hi There
print("hello".contains("ell"))     # true
print("hello".find("ll"))          # 2     (byte index, or -1 if absent)
print("hello".starts_with("he"))   # true
print("hello".ends_with("lo"))     # true
print("a,b".replace(",", ";"))     # a;b   (replaces every occurrence)
print("  hi  ".trim())             # "hi"  (also: lstrip / rstrip)
```

## Slicing

`s[lo:hi]` returns the half-open slice: it includes `lo` and excludes `hi`. Two
nice touches here: bounds are clamped to the length, and negative indices count
from the end, so a slice never goes out of range. Either bound can be omitted.

```lumen
let s = "hello"
print(s[0:2])     # "he"
print(s[1:])      # "ello"  (open end → to the end)
print(s[:3])      # "hel"   (open start → from 0)
print(s[-2:])     # "lo"    (negative start)
print(s[:])       # "hello" (a full copy)
```

(Slicing works the same way on [lists](lists.md#slicing).)

## Characters and codes

```lumen
print(ord("A"))        # 65    (first byte as an integer code)
print(chr(66))         # "B"   (code back to a 1-char string)
print(is_digit("7"))   # true
print(is_alpha("x"))   # true
print(is_space(" "))   # true
```

## Ordering

Strings compare lexicographically, byte by byte, so uppercase sorts before
lowercase:

```lumen
print("apple" < "banana")   # true
print("Dog" < "dog")        # true
```

## Reading input

`input(prompt?)` reads one line from standard input, strips the trailing newline,
and returns it. At end of file it returns `nil` instead. Pass a prompt and it
prints that first:

```lumen
let name = input("What is your name? ")
print(f"Hello, {name}!")
```

There's a full method list, with examples and results, over on
[the standard library page](stdlib.md#string-methods).
