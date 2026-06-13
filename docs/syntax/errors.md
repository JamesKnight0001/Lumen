# Errors: `try` / `catch` / `raise`

Lumen handles errors with three keywords. The model is small and predictable,
what you want when something has gone wrong.

## `raise`: signal an error

`raise <expr>` aborts with an error whose message is the string form of the
raised value.

```lumen
raise "something went wrong"
raise f"bad value: {x}"
```

## `try` / `catch`: handle it

A `try:` block runs protected. If anything inside faults, your own `raise` or a
built-in error like division by zero or an out-of-range index, control jumps to
`catch <name>:`, with `<name>` bound to the **error message string**. Otherwise
`catch` is skipped.

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

You don't only catch your own `raise`s. Runtime errors are caught the same way,
making `try`/`catch` a clean tool for "try this, skip it if it blows up":

```lumen
for x in [1, 0, 2]:
    try:
        print(10 / x)
    catch e:
        print("skipped: " + e)        # skipped: division by zero
```

## Nesting and re-raising

`try`/`catch` blocks nest, and a `catch` can `raise` again, pushing it to an
enclosing handler. That's how you wrap and rethrow:

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

An uncaught error makes Lumen print a friendly `error: <message>` with the
source location, then exit with status 1, like any runtime fault. Both backends
behave identically (the interpreter shows a source caret; a compiled `.exe`
shows `--> line N`).

## The catch binds a string, not an object

`catch e` binds `e` to the error **message string**, not a structured
exception object. That's deliberate: it keeps the model tiny, errors are just
messages. Need more structure? Build it into the message you `raise`.
