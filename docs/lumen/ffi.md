# Calling C libraries and DLLs (FFI)

Lumen can reach outside itself and call C-ABI functions in any shared library:
a `.dll` on Windows, the system C library, your own compiled C. This is how a
small language does big things: play sounds, open windows, draw with the
GPU, talk to the OS. On Windows it scales all the way up to **COM and
DirectX**.

This page is the conceptual tour. For the exact `cffi` calls, see
[the stdlib reference](../syntax/stdlib.md#the-cffi-module).

## The simplest case: a flat function

Declare what you're calling in an `extern` block, then call it like any
Lumen function:

```lumen
extern "C" from "kernel32.dll":
    fn Beep(freq: i64, dur: i64) -> i64

fn main():
    Beep(440, 200)    # a 440 Hz tone for 200 ms (Windows)
```

The native build links the library automatically; the interpreter loads it at
runtime with `LoadLibrary`/`GetProcAddress`. Same call, same result, both ways.

## Types cross the boundary by what you declare

Each parameter's declared type decides how it's passed to C:

| You write | C sees |
|-----------|--------|
| `i64` (or a bool) | a 64-bit integer |
| `str` | a NUL-terminated `char*` |
| `f64` / `f32` | a C `double` (in a float register) |
| `-> f64` | the return value is read as a double |
| `-> i64` (default) | the return value is read as an integer |
| `-> nil` | the return value is ignored |

So float math and graphics APIs work directly, `sqrt(2.0)`, `pow(2.0, 10.0)`,
and you can mix int and float arguments freely. The native backend takes up to
**16 arguments** (enough for `CreateWindowExA`'s twelve); the interpreter handles
up to four, so for wide-signature calls you build the program.

## Four building blocks, and what they unlock

As you move from "call a function" to "drive DirectX," the FFI gives you four
primitives, each layering on the last. They live in the `cffi` module.

1. **C buffers**: `cffi.cbuf(n)` makes a raw byte blob, and `set_*/get_*` read
   and write typed fields at byte offsets. This is how you build a C **struct**
   to pass by pointer, and how you receive data from an **out-parameter** (the
   "give me a pointer and I'll fill it in" idiom every Win32 and COM API uses).

2. **COM method calls**: `cffi.vcall(obj, slot, args, ret_kind)` calls a method
   on a COM object through its vtable. COM isn't flat functions; it's objects
   whose methods you call by *slot number*. This drives DirectX, Media
   Foundation, WIC, the Windows shell, audio, anything object-based on Windows.
   (`IUnknown` is always slots 0/1/2; an interface's own methods start at 3.)

3. **Callbacks**: `cffi.callback(fn)` turns a Lumen function into a C function
   pointer the operating system can call *back into*. That's what a window needs
   (a `WndProc`), and what enumeration and timer APIs use. Callbacks require a
   compiled program: a Lumen function in the interpreter has no machine-code
   address to hand out, so `lumen run` tells you to `lumen build`.

4. **Helpers**: `cffi.guid("...")` parses a COM interface ID string into the
   bytes Windows expects, and `cffi.str_ptr(s)` hands you a string's `char*` to
   drop into a struct field.

## The payoff

Put those together and you can open a real GPU-accelerated window and draw to it,
entirely in Lumen. See `examples/DirectX/`. Nothing to install: the DLLs
(`d2d1.dll`, `user32.dll`, and friends) ship with Windows; you just name them in
`extern` blocks. Smaller demos live in `examples/win32ui/` (native dialogs),
`examples/com/` (COM calls), and `examples/callback/` (a callback fired by the
OS).

## A reassurance

None of this slows your code down. The FFI machinery sits entirely on the cold
path; the [fast numeric codegen](performance.md) is untouched by it.
