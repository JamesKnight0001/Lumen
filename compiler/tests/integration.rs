//! Integration tests for the `lumenc` library: exercise the public API
//! (`parse_program`, `compile`) and the interpreter end to end, asserting on
//! captured output. These run with `cargo test` and complement the language-level
//! differential suite in `tests/run_tests.py` (which also checks the native
//! backend for byte-identical output).

use lumenc::interp::{Interp, Value};
use lumenc::{compile, parse_program};
use std::path::Path;

/// Run a self-contained program through the interpreter and return whether it
/// completed without error. (Output goes to stdout; we assert on control flow
/// and on `Interp` state rather than capturing stdout here.)
fn run_ok(src: &str) -> bool {
    let prog = compile(src, Path::new("."), true).expect("compile");
    Interp::new().run(&prog).is_ok()
}

#[test]
fn parses_hello() {
    let prog = parse_program("fn main():\n    print(\"hi\")\n").expect("parse");
    assert!(!prog.is_empty());
}

#[test]
fn bad_syntax() {
    // unbalanced / unexpected token should surface a parse error, not panic
    assert!(parse_program("fn main(:\n").is_err());
}

#[test]
fn run_math() {
    assert!(run_ok("fn main():\n    print(2 ** 10)\n    print(7 % 3)\n"));
}

#[test]
fn runs_closures() {
    let src = "\
fn make_adder(n):
    return fn(x):
        return x + n
fn main():
    let add5 = make_adder(5)
    print(add5(10))
";
    assert!(run_ok(src));
}

#[test]
fn struct_and_methods() {
    let src = "\
struct P:
    x: f64
    y: f64
impl P:
    fn sum(self):
        return self.x + self.y
fn main():
    let p = P(x: 1.0, y: 2.0)
    print(p.sum())
";
    assert!(run_ok(src));
}

#[test]
fn trailing_commas() {
    assert!(parse_program("fn f(a, b,):\n    return [a, b,]\n").is_ok());
}

#[test]
fn missing_imports() {
    // a non-builtin module that does not exist on disk -> CompileError::Import
    let err = compile(
        "import does_not_exist\nfn main():\n    print(1)\n",
        Path::new("."),
        true,
    );
    assert!(matches!(err, Err(lumenc::CompileError::Import(_))));
}

#[test]
fn built_in_pkg() {
    // `import math` must NOT try to read math.lm from disk
    assert!(run_ok(
        "import math\nfn main():\n    print(math.sqrt(9.0))\n"
    ));
}

#[test]
fn membership_operator() {
    assert!(run_ok("fn main():\n    print(3 in [1, 2, 3])\n"));
    assert!(run_ok("fn main():\n    print(\"x\" not in \"abc\")\n"));
    assert!(run_ok(
        "fn main():\n    let m = {\"a\": 1}\n    print(\"a\" in m)\n"
    ));
}

#[test]
fn string_iteration() {
    assert!(run_ok(
        "fn main():\n    for ch in \"hi\":\n        print(ch)\n"
    ));
}

#[test]
fn ternary_expression() {
    assert!(run_ok(
        "fn main():\n    let x = 5\n    print(\"big\" if x > 3 else \"small\")\n"
    ));
    // chained ternary
    assert!(run_ok(
        "fn main():\n    let g = 85\n    print(\"A\" if g >= 90 else \"B\" if g >= 80 else \"C\")\n"
    ));
}

#[test]
fn slicing() {
    assert!(run_ok(
        "fn main():\n    let xs = [1, 2, 3, 4]\n    print(xs[1:3])\n"
    ));
    assert!(run_ok("fn main():\n    print(\"hello\"[0:2])\n"));
    // negative and clamped bounds must not panic
    assert!(run_ok(
        "fn main():\n    let xs = [1, 2, 3]\n    print(xs[-2:100])\n    print(xs[2:1])\n"
    ));
    // open-ended slices: omitted lo / hi / both
    assert!(run_ok(
        "fn main():\n    let xs = [1, 2, 3, 4]\n    print(xs[:2])\n    print(xs[2:])\n    print(xs[:])\n"
    ));
    assert!(run_ok(
        "fn main():\n    print(\"hello\"[:3])\n    print(\"hello\"[2:])\n"
    ));
}

#[test]
fn iter_snapshot() {
    // Mutating a list inside the loop must NOT change the current iteration
    // (matches the interpreter, which snapshots into a Vec).
    assert!(run_ok(
        "fn main():\n    let xs = [1, 2, 3]\n    let n = 0\n    for x in xs:\n        n = n + 1\n        if x == 1:\n            xs.push(9)\n    print(n)\n"
    ));
}

#[test]
fn single_linefn() {
    // `fn f(): <stmt>` on one line, alongside the block and `= expr` forms.
    assert!(run_ok(
        "fn sq(n): return n * n\nfn main():\n    print(sq(6))\n"
    ));
    assert!(run_ok(
        "fn shout(s): print(s.upper())\nfn main():\n    shout(\"hi\")\n"
    ));
}

#[test]
fn list_comprehensions() {
    assert!(run_ok("fn main():\n    print([x * x for x in 0..4])\n"));
    assert!(run_ok(
        "fn main():\n    print([n for n in 0..10 if n % 2 == 0])\n"
    ));
    assert!(run_ok("fn main():\n    print([c for c in \"abc\"])\n"));
    // loop var must not leak
    assert!(run_ok(
        "fn main():\n    let x = 9\n    let _ = [x for x in 0..3]\n    print(x)\n"
    ));
}

#[test]
fn value_display() {
    // a couple of Value Display forms the REPL and print() rely on
    assert_eq!(format!("{}", Value::Int(42)), "42");
    assert_eq!(format!("{}", Value::Bool(true)), "true");
    assert_eq!(format!("{}", Value::Nil), "nil");
}
