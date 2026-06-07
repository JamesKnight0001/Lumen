# Maps

A map associates keys with values, much like a dictionary. Both keys and values
can be any type, and here's the part worth remembering: **insertion order is
preserved**. That means printing and iterating a map are fully deterministic,
and identical across both backends.

```lumen
let ages = {"ana": 30, "bo": 25}
print(ages["ana"])       # 30      (look up by key)
ages["cy"] = 41          # insert a new entry
ages["ana"] = 31         # overwrite (keeps the original position)
print(len(ages))         # 3
```

The empty map is `{}`. Integer keys work just as well as string keys:

```lumen
let squares = {1: 1, 2: 4, 3: 9}
print(squares[2])        # 4
```

## Safe lookup

Indexing a **missing** key with `m[key]` is an error, plain and simple. So when a
key might not be there, reach for `get` with a default, or check ahead of time
with `has`:

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
print(ages.remove("bo")) # 25    (deletes the key, returns its value)
```

## Iterating

`for k in m:` walks the **keys**, always in insertion order:

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

The full method list lives on
[the standard library page](stdlib.md#map-dictionary-methods).
