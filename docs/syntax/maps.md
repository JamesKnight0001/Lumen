# Maps

A map associates keys with values, like a dictionary. Keys and values can be
any type, and **insertion order is preserved**. So printing and iterating a map
are fully deterministic and identical across both backends.

```lumen
let ages = {"ana": 30, "bo": 25}
print(ages["ana"])       # 30      (look up by key)
ages["cy"] = 41          # insert a new entry
ages["ana"] = 31         # overwrite (keeps the original position)
print(len(ages))         # 3
```

The empty map is `{}`. Integer keys work as well as string keys:

```lumen
let squares = {1: 1, 2: 4, 3: 9}
print(squares[2])        # 4
```

Numeric keys follow the same rule as `==`: an integer and a whole-valued float
that compare equal are the **same key**, so `m[1]` and `m[1.0]` reach the same
slot. (Non-whole floats like `1.5` are distinct keys.)

```lumen
let m = {}
m[1] = "a"
m[1.0] = "b"             # overwrites the same key
print(m[1])              # b
print(len(m))            # 1
```

## Safe lookup

Indexing a **missing** key with `m[key]` is an error. When a key might not be
there, use `get` with a default, or check first with `has`:

```lumen
let ages = {"ana": 31}
print(ages.has("bo"))        # false
print(ages.get("bo"))        # nil       (no default → nil if missing)
print(ages.get("bo", 0))     # 0         (your default)
```

## Reading the contents

```lumen
let ages = {"ana": 31, "bo": 25, "cy": 41}
print(ages.keys())       # ["ana", "bo", "cy"]   (insertion order)
print(ages.values())     # [31, 25, 41]
print(ages.items())      # [["ana", 31], ["bo", 25], ["cy", 41]]
print(ages.remove("bo")) # 25    (deletes the key, returns its value)
```

## Iterating

`for k in m:` walks the **keys** in insertion order:

```lumen
# count word frequencies
let counts = {}
for word in ["a", "b", "a"]:
    if counts.has(word):
        counts[word] = counts[word] + 1
    else:
        counts[word] = 1
print(counts)            # {"a": 2, "b": 1}
```

To walk keys and values together, iterate `items()` and
[destructure](variables.md#destructuring) each pair:

```lumen
for name, age in ages.items():
    print(f"{name} is {age}")
```

Full method list: [the standard library page](stdlib.md#map-dictionary-methods).
