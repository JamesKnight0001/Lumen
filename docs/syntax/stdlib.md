# The standard library

This is the reference for everything Lumen hands you out of the box. The global
builtins and the string, list, and map methods need no import at all; the modules
down at the bottom (`math`, `os`, `rand`, `time`, `json`, `cffi`) come in with an
`import`. Whichever you reach for, every entry behaves identically under the
interpreter and the compiled binary.

## Global builtins

| Function | Does | Example → result |
|----------|------|------------------|
| `print(...)` | print values (space-separated) and a newline | `print(1, "x")` → `1 x` |
| `len(x)` | length of a string, list, or map | `len([1,2,3])` → `3` |
| `str(v)` | value → its string form | `str(3.5)` → `"3.5"` |
| `int(v)` | float/string → integer (truncating) | `int(3.9)` → `3` |
| `float(v)` | int → float | `float(2)` → `2.0` |
| `abs(n)` | absolute value (int or float) | `abs(-7)` → `7` |
| `min(list)` | smallest numeric element | `min([3,1,4])` → `1` |
| `max(list)` | largest numeric element | `max([3,1,4])` → `4` |
| `sum(list)` | sum (int if all ints, else float) | `sum([1,2,3])` → `6` |
| `range(n)` | list of `0..n` | `range(4)` → `[0,1,2,3]` |
| `range(lo,hi)` | list of `lo..hi` | `range(2,5)` → `[2,3,4]` |
| `type(v)` | runtime type name as a string | `type(3.5)` → `"f64"` |
| `assert(cond)` | abort the program if `cond` is falsey | `assert(x > 0)` | - |
| `ord(c)` | first byte of a string as a code | `ord("A")` → `65` |
| `chr(n)` | code → 1-character string | `chr(65)` → `"A"` |
| `is_digit(c)` | first char is `0`–`9`? | `is_digit("7")` → `true` |
| `is_alpha(c)` | first char is a letter? | `is_alpha("x")` → `true` |
| `is_space(c)` | first char is whitespace? | `is_space(" ")` → `true` |
| `input(prompt?)` | read a line (newline stripped); `nil` at EOF | `input("name? ")` → `"Ada"` |
| `drop(x)` | release a heap value now (optional; no-op in interpreter) | `drop(big_list)` | - |

## String methods

| Method | Does | Example → result |
|--------|------|------------------|
| `s.len()` | number of characters | `"hi".len()` → `2` |
| `s.upper()` | uppercase copy | `"hi".upper()` → `"HI"` |
| `s.lower()` | lowercase copy | `"HI".lower()` → `"hi"` |
| `s.title()` | title-case each word | `"hi there".title()` → `"Hi There"` |
| `s.contains(sub)` | substring test | `"hello".contains("ell")` → `true` |
| `s.find(sub)` | byte index of first match, or `-1` | `"hello".find("ll")` → `2` |
| `s.replace(old,new)` | replace every occurrence | `"a,b".replace(",",";")` → `"a;b"` |
| `s.starts_with(p)` | prefix test | `"hello".starts_with("he")` → `true` |
| `s.ends_with(p)` | suffix test | `"hello".ends_with("lo")` → `true` |
| `s.lstrip()` | strip leading whitespace | `"  hi".lstrip()` → `"hi"` |
| `s.rstrip()` | strip trailing whitespace | `"hi  ".rstrip()` → `"hi"` |
| `s.trim()` | strip both ends | `"  hi  ".trim()` → `"hi"` |
| `s.split(sep)` | split into a list | `"a,b".split(",")` → `["a","b"]` |
| `s.repeat(n)` | concatenate `s` n times | `"ab".repeat(3)` → `"ababab"` |
| `s.join(list)` | join a list of strings with `s` as glue | `",".join(["a","b"])` → `"a,b"` |

f-strings interpolate any expression you give them: `f"sum is {a + b}"`. For
slicing and the `ord`/`chr` pair, head over to [strings](strings.md).

## List methods

| Method | Does | Example → result |
|--------|------|------------------|
| `xs.len()` | number of elements | `[1,2,3].len()` → `3` |
| `xs.push(v)` | append (mutates) | `xs.push(4)` | - |
| `xs.pop()` | remove + return the last | `xs.pop()` → last element |
| `xs.insert(i,v)` | insert at index `i` (mutates) | `xs.insert(0,9)` | - |
| `xs.contains(v)` | membership test | `[1,2].contains(2)` → `true` |
| `xs.index(v)` | index of first match, or `-1` | `[10,20,30].index(20)` → `1` |
| `xs.count(v)` | how many equal `v` | `[1,2,2,3].count(2)` → `2` |
| `xs.map(f)` | new list of `f(x)` | `[1,2,3].map(double)` → `[2,4,6]` |
| `xs.filter(p)` | new list where `p(x)` is true | `[1,2,3,4].filter(is_even)` → `[2,4]` |
| `xs.join(sep)` | join string elements | `["a","b"].join("-")` → `"a-b"` |
| `xs.sort()` | ascending numeric sort, in place | `xs.sort()` | - |
| `xs.reverse()` | reverse, in place | `xs.reverse()` | - |
| `xs[i]` / `xs[lo:hi]` | index / slice | `[10,20][1]` → `20` |

For slicing details and comprehensions, see [lists](lists.md).

## Map (dictionary) methods

A map literal is `{key: value, ...}`, and the empty map is `{}`. You index with
`m[key]` and assign with `m[key] = value`. Keys always hold their insertion order.

| Method | Does | Example → result |
|--------|------|------------------|
| `m[key]` | look up (error if missing) | `{"a":1}["a"]` → `1` |
| `m[key] = v` | insert or overwrite | `m["b"] = 2` | - |
| `len(m)` / `m.len()` | number of entries | `len({"a":1})` → `1` |
| `m.has(key)` | key present? | `{"a":1}.has("a")` → `true` |
| `m.contains(key)` | alias for `has` | `{"a":1}.contains("a")` → `true` |
| `m.get(key)` | value, or `nil` if missing | `{"a":1}.get("z")` → `nil` |
| `m.get(key, default)` | value, or `default` if missing | `{"a":1}.get("z", 0)` → `0` |
| `m.keys()` | keys, in insertion order | `{"a":1,"b":2}.keys()` → `["a","b"]` |
| `m.values()` | values, in insertion order | `{"a":1,"b":2}.values()` → `[1,2]` |
| `m.remove(key)` | delete a key, return its value | `m.remove("a")` → the value |

Iterating `for k in m:` yields the keys. For the full story, see [maps](maps.md).

---

## The `math` module (`import math`)

Trig works in radians, both in and out. Everything returns a float **except**
`gcd`, `lcm`, and `factorial`, which hand back integers.

```lumen
import math
print(math.sqrt(2.0))       # 1.4142135623730951
print(math.floor(3.7))      # 3.0
print(math.pow(2.0, 10.0))  # 1024.0
```

Roots & powers: `sqrt` `cbrt` `pow` `exp` `expm1` `log` `log1p` `log2` `log10`.
Trig: `sin` `cos` `tan` `asin` `acos` `atan` `atan2(y,x)` `sinh` `cosh` `tanh`.
Rounding: `floor` `ceil` `round` `trunc` `sign` `abs`.
Misc: `min(x,y)` `max(x,y)` `hypot(x,y)` `fmod(x,y)` `copysign(x,y)` `deg(x)`
`rad(x)`. Predicates: `isnan` `isinf` `isfinite`.
**Integer-returning:** `gcd(a,b)` `lcm(a,b)` `factorial(n)` (`0` for `n < 0`).

Constants are zero-arg functions, so remember to call them: `math.pi()`,
`math.e()`, `math.tau()`, `math.inf()`.

## The `os` module (`import os`)

Files, environment, and process, all in one place. There's a simple convention
worth internalizing: reads (along with `getenv`/`cwd`) give you the value or
`nil`, while mutating ops return a `bool`. Directory listings come back sorted, so
results stay deterministic.

```lumen
import os
os.mkdir("out")
os.write("out/log.txt", "hello\n")        # overwrite → true
os.append("out/log.txt", "world\n")       # → true
print(os.read("out/log.txt"))             # "hello\nworld\n"  (nil if missing)
print(os.listdir("out"))                  # ["log.txt"]  (sorted)
os.remove("out/log.txt")
os.rmdir("out")
```

| Function | Returns |
|----------|---------|
| `os.read(path)` | file text, or `nil` |
| `os.write(path, s)` / `os.append(path, s)` | bool |
| `os.exists(path)` / `os.is_file(path)` / `os.is_dir(path)` | bool |
| `os.remove(path)` / `os.mkdir(path)` / `os.rmdir(path)` | bool |
| `os.rename(a, b)` | bool |
| `os.listdir(path)` | sorted list of names, or `nil` |
| `os.getenv(name)` / `os.setenv(name, v)` | string-or-`nil` / bool |
| `os.cwd()` | current directory, or `nil` |
| `os.time()` / `os.clock()` / `os.getpid()` | integers (epoch seconds / ms / pid) |
| `os.sep()` / `os.platform()` | `"\\"`-or-`"/"` / `"windows"`,`"linux"`,`"macos"` |
| `os.system(cmd)` | run via the shell, return exit code (int) |
| `os.exec(cmd)` | run via the shell, return captured stdout (string, `nil` on failure) |
| `os.exit(code)` | terminate immediately (never returns) |
| `os.args()` | command-line arguments (program name first) |

`os.time/clock/getpid/cwd` and `os.system/exec` all depend on the environment, so
they sit outside the byte-identical suite. Even so, both backends invoke the same
platform shell, which means a given command behaves the same either way.

## The `rand` module (`import rand`)

A seedable, fully **deterministic** generator (SplitMix64). Same seed, same
sequence, identical across both backends, which is exactly what makes seeded
programs reproducible.

```lumen
import rand
rand.seed(42)
print(rand.int(1, 6))   # a dice roll - same every run for this seed
print(rand.float())     # 0.0 <= x < 1.0
```

`rand.seed(n)`, `rand.int(lo, hi)` (inclusive), and `rand.float()` (`[0.0, 1.0)`).
Want fresh variation from one run to the next? Seed from the clock:
`rand.seed(os.time())`.

## The `time` module (`import time`)

```lumen
import time
print(time.format(0))   # "1970-01-01 00:00:00"  (UTC, deterministic)
time.sleep(100)         # pause 100 ms
```

`time.now()` (epoch milliseconds, int), `time.format(secs)` (UTC
`"YYYY-MM-DD HH:MM:SS"`), `time.sleep(ms)`.

## The `json` module (`import json`)

Compact, deterministic JSON. The output carries no spaces, and numbers print the
same way `print` renders them, so both backends produce byte-for-byte identical
strings.

```lumen
import json
print(json.stringify({"name": "lumen", "tags": ["fast", "small"]}))
# -> {"name":"lumen","tags":["fast","small"]}

let data = json.parse("{\"ok\":true,\"n\":3}")   # nil if invalid
print(data["n"])                                  # 3
```

`json.stringify(v)` (lists→arrays, maps→objects, `nil`→`null`, functions/structs
→`null`) and `json.parse(s)` (objects→maps, arrays→lists, or `nil` if invalid).

## The `cffi` module (`import cffi`)

Tools for calling C libraries and DLLs: raw buffers for C **structs** and
**out-parameters**, **COM** method calls, and **callbacks**. The big-picture tour
lives in [the FFI page](../lumen/ffi.md); what follows here is the call list.

A `cbuf` is a fixed-size byte buffer. You write typed fields at byte offsets, then
hand it to an `extern` function, which receives its data pointer. Fields are
little-endian, and out-of-range access raises a catchable error rather than
corrupting anything.

| Call | Does |
|------|------|
| `cffi.cbuf(n)` | allocate a zero-filled `n`-byte buffer |
| `cffi.len(b)` / `cffi.addr(b)` | size in bytes / raw data pointer as an int |
| `cffi.set_i8/i16/i32/i64(b, off, v)` | write a signed integer field |
| `cffi.set_ptr(b, off, v)` | write a 64-bit pointer/handle field |
| `cffi.set_f32/f64(b, off, v)` | write a float/double field |
| `cffi.get_i8/i16/i32/i64(b, off)` | read a signed integer field |
| `cffi.get_ptr(b, off)` / `cffi.get_f32/f64(b, off)` | read pointer / float field |
| `cffi.peek_i64/i32(addr)` | read an int through a raw pointer (e.g. a callback's `lParam`) |
| `cffi.poke_i64/i32(addr, v)` | write an int through a raw pointer |
| `cffi.str_ptr(s)` | a Lumen string's `char*`, as an int (to store in a struct field) |
| `cffi.guid(s)` | parse `"xxxxxxxx-xxxx-…"` into a 16-byte cbuf in Windows GUID layout |
| `cffi.vcall(obj, slot, args, ret_kind)` | call COM method `slot` on `obj` through its vtable |
| `cffi.callback(fn)` | turn a Lumen `fn` into a C function pointer (compiled programs only) |

A note on `cffi.vcall`: `args` is a cbuf of 8-byte argument words (or `nil`), and
`ret_kind` is `0` for an int/HRESULT/pointer return or `1` for `f64`. The `this`
pointer is passed automatically. `IUnknown` always occupies slots `0`=QueryInterface,
`1`=AddRef, `2`=Release; an interface's own methods begin at slot `3`.

```lumen
import cffi
extern "C" from "kernel32.dll":
    fn GetSystemTime(p: i64) -> i64       # fills a SYSTEMTIME struct via pointer

fn main():
    let st = cffi.cbuf(16)                # SYSTEMTIME is 8 × u16
    GetSystemTime(st)                     # pass the buffer as the struct pointer
    print(cffi.get_i16(st, 0))            # wYear
    print(cffi.get_i16(st, 6))            # wDay
```

Worked demos: `examples/com/` (COM), `examples/callback/` (callbacks),
`examples/DirectX/` (a real GPU window).

---

## Numeric semantics, in one place

- Integer `/` is integer division: `7 / 2 == 3`. When you want true division,
  reach for floats: `7.0 / 2.0 == 3.5`.
- `sum`/`min`/`max` operate on numeric lists; `sum` stays an integer when every
  element is an integer, and otherwise promotes to float.
- Comparisons and equality cross the int/float line freely (`1 == 1.0` is `true`).
- Strings compare lexicographically by byte, and uppercase sorts before lowercase.
- Integers are [48-bit and wrap on overflow](values.md#integers-are-48-bit).
