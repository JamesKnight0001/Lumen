# Structs and methods

A `struct` groups named fields into your own type. Each field names a type
(`f64`, say), which documents intent and gives the compiler something to work
with.

```lumen
struct Vec2:
    x: f64
    y: f64
```

## Constructing and reading

Build a struct with named fields, `Type(field: value, ...)`:

```lumen
let v = Vec2(x: 3.0, y: 4.0)
print(v.x)        # 3.0
print(v.y)        # 4.0
```

Printing a struct emits fields in declaration order, so output is predictable.

## Methods: `impl`

Behavior goes in an `impl` block. Methods take `self` as the first parameter
and use dot syntax. A method can return a new struct, which makes fluent,
value-style APIs natural:

```lumen
import math

struct Vec2:
    x: f64
    y: f64

impl Vec2:
    fn length(self) -> f64:
        return math.sqrt(self.x * self.x + self.y * self.y)

    fn scaled(self, k):
        return Vec2(x: self.x * k, y: self.y * k)

fn main():
    let v = Vec2(x: 3.0, y: 4.0)
    print(v.length())        # 5.0
    let w = v.scaled(2.0)
    print(w.x)               # 6.0
    print(w.length())        # 10.0
```

The `-> f64` on `length` is an optional return-type annotation. Leave it off (as
`scaled` does) and the method behaves the same.

## How structs relate to the rest

- `type(v)` on a struct value returns `"struct"`.
- Structs are heap objects (like lists and maps) and are
  [garbage-collected](../lumen/memory.md) when unreachable.
- Need a C struct to hand to a DLL? That's different: a raw byte buffer via the
  [`cffi` module](stdlib.md#the-cffi-module), not a Lumen `struct`.
