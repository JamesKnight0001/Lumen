# Errors: `try` / `catch` / `raise`

Lumen handles errors with three keywords, and that's it. The model is small and
predictable, which is exactly what you want when something has already gone wrong.

## `raise`: signal an error

`raise <expr>` aborts with an error whose message is the string form of whatever
value you raised.

```lumen
raise "something went wrong"
raise f"bad value: {x}"
```

## `try` / `catch`: handle it

A `try:` block runs protected. If anything inside it faults, whether that's your
own `raise` or a built-in runtime error like division by zero or an out-of-range
index, control jumps straight to the `catch <name>:` block, with `<name>` bound to
the **error message string**. If nothing faults, the `catch` is simply skipped.

```lumen
fn parse_positive(s):
    let n = int(s)
    if n < 0:
        raise "must be positive: " + s
    return n

fn main():
    try:
        print(parse_positive("42"))   # 42
        print(parse_positive("-1"))   # raises here
        print("unreached")            # never runs
    catch e:
        print("error: " + e)          # error: must be positive: -1
```

## Built-in faults are catchable too

You don't only catch your own `raise`s. Runtime errors are caught the very same
way, which makes `try`/`catch` a clean tool for "try this, skip it if it blows
up":

```lumen
for x in [1, 0, 2]:
    try:
        print(10 / x)
    catch e:
        print("skipped: " + e)        # skipped: division by zero
```

## Nesting and re-raising

`try`/`catch` blocks nest, and a `catch` can `raise` again to push the error out
to an enclosing handler. That's how you wrap and rethrow:

```lumen
try:
    try:
        raise "inner"
    catch e:
        raise "wrapped: " + e     # hand it to the outer handler
catch e:
    print(e)                      # wrapped: inner
```

## What an uncaught error does

If an error reaches the top level with no handler, Lumen prints a friendly
`error: <message>` along with the source location and exits with status 1, just
like any other runtime fault. Both backends do this identically (the interpreter
shows a source caret; a compiled `.exe` shows `--> line N`).

## The catch binds a string, not an object

`catch e` gives you `e` as the error **message string**, not a structured
exception object. That's a deliberate choice, and it keeps the model tiny: errors
are just messages. If you need more structure, build it into the message you
`raise`.
