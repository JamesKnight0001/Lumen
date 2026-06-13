//! Escape analysis for arena allocation.
//!
//! Finds `let x = <list|map|struct literal/ctor>` bindings that provably never
//! escape their function, so the backend can bump-allocate them in a per-call
//! arena and free them on return instead of burdening the GC.
//!
//! Soundness over completeness: a wrong "non-escaping" verdict is a
//! use-after-free, so a binding qualifies only when EVERY use is on a tight
//! whitelist. Anything we don't understand disqualifies it (stays a heap object).
//!
//! `x` is disqualified if it is ever returned, passed to a call/method, stored
//! into another value, captured by a closure, aliased (`y = x`), or reassigned.
//! Safe positions: method receiver `x.m(..)`, index/field read or assign target,
//! `len/print/str/..` arg, `for _ in x`, and plain reads in arithmetic/conditions.

use crate::ast::{Expr, FStrPart, Item, Program, Stmt};
use std::collections::HashSet;

/// (function name, local name) pairs whose init collection may be arena-placed.
/// Name-keyed: any reassigned/shadowed binding is disqualified, so a name maps
/// to exactly one alloc.
pub type ArenaSet = HashSet<(String, String)>;

/// Methods that never retain their list/map receiver past the call.
fn safe_method(name: &str) -> bool {
    matches!(
        name,
        "push" | "pop" | "get" | "set" | "keys" | "values" | "items" | "has"
            | "remove" | "clear" | "len" | "contains" | "insert" | "sort"
            | "reverse" | "index" | "find" | "count"
    )
}

/// Free builtins that take the value but don't store it.
fn safe_builtin(name: &str) -> bool {
    matches!(name, "len" | "print" | "println" | "str" | "type" | "drop" | "repr")
}

pub fn analyze(prog: &Program) -> ArenaSet {
    let mut out = ArenaSet::new();
    // `let x = Name(...)` for a struct Name is a fresh alloc like a list literal.
    let structs: HashSet<String> = prog
        .iter()
        .filter_map(|i| match i {
            Item::Struct(s) => Some(s.name.clone()),
            _ => None,
        })
        .collect();
    // Free functions only, keyed by name (matches llvmgen's ctx.func). Struct
    // methods are skipped: dispatch can alias their locals beyond what this
    // intra-procedural pass sees, so they stay on the heap.
    for item in prog {
        if let Item::Fn(f) = item {
            analyze_fn(&f.name, &f.body, &structs, &mut out);
        }
    }
    out
}

fn analyze_fn(fname: &str, body: &[Stmt], structs: &HashSet<String>, out: &mut ArenaSet) {
    let mut candidates: Vec<String> = Vec::new();
    collect_candidates(body, structs, &mut candidates);

    // A candidate qualifies only if no use escapes and it's bound exactly once.
    for name in &candidates {
        let mut esc = Escapes {
            name,
            escaped: false,
            assigns: 0,
        };
        esc.walk_block(body);
        if !esc.escaped && esc.assigns <= 1 {
            out.insert((fname.to_string(), name.clone()));
        }
    }
}

/// A list/map literal, comprehension, or struct ctor `Name(...)`: a fresh heap
/// object we could arena-place.
fn fresh_alloc(e: &Expr, structs: &HashSet<String>) -> bool {
    match e {
        Expr::List(_) | Expr::Map(_) | Expr::ListComp { .. } => true,
        Expr::Call { callee, .. } | Expr::NamedCall { callee, .. } => {
            matches!(&**callee, Expr::Ident(n) if structs.contains(n))
        }
        _ => false,
    }
}

fn collect_candidates(body: &[Stmt], structs: &HashSet<String>, out: &mut Vec<String>) {
    for s in body {
        match s {
            Stmt::Let { name, value, .. } => {
                if fresh_alloc(value, structs) {
                    out.push(name.clone());
                }
            }
            Stmt::If { then, elifs, els, .. } => {
                collect_candidates(then, structs, out);
                for (_, b) in elifs {
                    collect_candidates(b, structs, out);
                }
                if let Some(b) = els {
                    collect_candidates(b, structs, out);
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } => {
                collect_candidates(body, structs, out)
            }
            Stmt::Try { body, catch_body, .. } => {
                collect_candidates(body, structs, out);
                collect_candidates(catch_body, structs, out);
            }
            _ => {}
        }
    }
}

struct Escapes<'a> {
    name: &'a str,
    escaped: bool,
    assigns: usize,
}

impl Escapes<'_> {
    fn walk_block(&mut self, body: &[Stmt]) {
        for s in body {
            self.walk_stmt(s);
        }
    }

    fn walk_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { name, value, .. } => {
                // `let y = x` aliases x -> escape.
                if let Expr::Ident(n) = value {
                    if n == self.name {
                        self.escaped = true;
                    }
                }
                self.walk_expr(value);
                // a later `let x = ...` rebinds our name; count it as an assign.
                if name == self.name {
                    self.assigns += 1;
                }
            }
            Stmt::Assign { target, value } => {
                if let Expr::Ident(t) = target {
                    if t == self.name {
                        self.assigns += 1;
                    }
                }
                if let Expr::Ident(v) = value {
                    if v == self.name {
                        // x flows into another binding -> escape (x = x is a no-op).
                        if !matches!(target, Expr::Ident(t) if t == self.name) {
                            self.escaped = true;
                        }
                    }
                }
                self.check_store(target, value);
                self.walk_target(target);
                self.walk_expr(value);
            }
            Stmt::ExprStmt(e) => self.walk_expr(e),
            Stmt::Return(Some(e)) => {
                if self.mentions(e) {
                    self.escaped = true;
                }
                self.walk_expr(e);
            }
            Stmt::Return(None) => {}
            Stmt::If { cond, then, elifs, els } => {
                self.walk_expr(cond);
                self.walk_block(then);
                for (c, b) in elifs {
                    self.walk_expr(c);
                    self.walk_block(b);
                }
                if let Some(b) = els {
                    self.walk_block(b);
                }
            }
            Stmt::While { cond, body } => {
                self.walk_expr(cond);
                self.walk_block(body);
            }
            Stmt::For { iter, body, .. } => {
                // `for _ in x` is a safe read; x inside a bigger iter is checked.
                if !matches!(iter, Expr::Ident(n) if n == self.name) {
                    self.walk_expr(iter);
                }
                self.walk_block(body);
            }
            Stmt::Try { body, catch_body, .. } => {
                self.walk_block(body);
                self.walk_block(catch_body);
            }
            Stmt::Raise(e) => {
                if self.mentions(e) {
                    self.escaped = true;
                }
                self.walk_expr(e);
            }
            Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => {}
        }
    }

    // store of x into another object's slot/field -> escape (x into itself is ok)
    fn check_store(&mut self, target: &Expr, value: &Expr) {
        let stores_x = matches!(value, Expr::Ident(n) if n == self.name) || self.mentions(value);
        if !stores_x {
            return;
        }
        match target {
            Expr::Index { obj, .. } | Expr::Field { obj, .. } => {
                if !matches!(&**obj, Expr::Ident(n) if n == self.name) {
                    self.escaped = true;
                }
            }
            _ => {}
        }
    }

    fn walk_target(&mut self, target: &Expr) {
        match target {
            Expr::Index { obj, index } => {
                // x[i] = .. : obj==x is a safe in-place write; still walk index.
                if !matches!(&**obj, Expr::Ident(n) if n == self.name) {
                    self.walk_expr(obj);
                }
                self.walk_expr(index);
            }
            Expr::Field { obj, .. } => {
                if !matches!(&**obj, Expr::Ident(n) if n == self.name) {
                    self.walk_expr(obj);
                }
            }
            _ => self.walk_expr(target),
        }
    }

    /// Does this expression mention our name anywhere (for return/raise/store)?
    fn mentions(&self, e: &Expr) -> bool {
        match e {
            Expr::Ident(n) => n == self.name,
            Expr::Unary { expr, .. } => self.mentions(expr),
            Expr::Binary { lhs, rhs, .. } => self.mentions(lhs) || self.mentions(rhs),
            Expr::Index { obj, index } => self.mentions(obj) || self.mentions(index),
            Expr::Slice { obj, lo, hi } => {
                self.mentions(obj)
                    || lo.as_ref().is_some_and(|x| self.mentions(x))
                    || hi.as_ref().is_some_and(|x| self.mentions(x))
            }
            Expr::Field { obj, .. } => self.mentions(obj),
            Expr::Call { callee, args } => {
                self.mentions(callee) || args.iter().any(|a| self.mentions(a))
            }
            Expr::Method { obj, args, .. } => {
                self.mentions(obj) || args.iter().any(|a| self.mentions(a))
            }
            Expr::List(xs) => xs.iter().any(|x| self.mentions(x)),
            Expr::Map(kv) => kv.iter().any(|(k, v)| self.mentions(k) || self.mentions(v)),
            Expr::Range { lo, hi } => self.mentions(lo) || self.mentions(hi),
            Expr::IfElse { cond, then, els } => {
                self.mentions(cond) || self.mentions(then) || self.mentions(els)
            }
            _ => false,
        }
    }

    fn walk_expr(&mut self, e: &Expr) {
        match e {
            // A plain Ident read (arithmetic/condition) is safe; escapes are
            // flagged at the specific escaping sites below.
            Expr::Ident(_) | Expr::Int(_) | Expr::Float(_) | Expr::Str(_)
            | Expr::Bool(_) | Expr::Nil | Expr::SelfExpr => {}

            Expr::Unary { expr, .. } => self.walk_expr(expr),
            Expr::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs);
                self.walk_expr(rhs);
            }
            // Passing x to any call escapes, unless it's a non-retaining builtin.
            Expr::Call { callee, args } => {
                let safe = matches!(&**callee, Expr::Ident(n) if safe_builtin(n));
                for a in args {
                    if matches!(a, Expr::Ident(n) if n == self.name) {
                        if !safe {
                            self.escaped = true;
                        }
                    } else {
                        self.walk_expr(a);
                    }
                }
                self.walk_expr(callee);
            }
            Expr::NamedCall { callee, args } => {
                for (_, a) in args {
                    if self.mentions(a) {
                        self.escaped = true;
                    }
                }
                self.walk_expr(callee);
            }
            // x.method(args): receiver is safe only for non-retaining methods;
            // args always escape.
            Expr::Method { obj, name, args } => {
                let recv_x = matches!(&**obj, Expr::Ident(n) if n == self.name);
                if recv_x && !safe_method(name) {
                    self.escaped = true;
                }
                if !recv_x {
                    self.walk_expr(obj);
                }
                for a in args {
                    if matches!(a, Expr::Ident(n) if n == self.name) {
                        self.escaped = true;
                    } else {
                        self.walk_expr(a);
                    }
                }
            }
            Expr::Field { obj, .. } => {
                if !matches!(&**obj, Expr::Ident(n) if n == self.name) {
                    self.walk_expr(obj);
                }
            }
            Expr::Index { obj, index } => {
                if !matches!(&**obj, Expr::Ident(n) if n == self.name) {
                    self.walk_expr(obj);
                }
                self.walk_expr(index);
            }
            Expr::Slice { obj, lo, hi } => {
                if !matches!(&**obj, Expr::Ident(n) if n == self.name) {
                    self.walk_expr(obj);
                }
                if let Some(x) = lo {
                    self.walk_expr(x);
                }
                if let Some(x) = hi {
                    self.walk_expr(x);
                }
            }
            // Storing x into a fresh literal escapes (the literal may leave).
            Expr::List(xs) => {
                for x in xs {
                    if self.mentions(x) {
                        self.escaped = true;
                    }
                }
            }
            Expr::Map(kv) => {
                for (k, v) in kv {
                    if self.mentions(k) || self.mentions(v) {
                        self.escaped = true;
                    }
                }
            }
            Expr::Range { lo, hi } => {
                self.walk_expr(lo);
                self.walk_expr(hi);
            }
            Expr::IfElse { cond, then, els } => {
                // x as a branch result flows out -> escape.
                if self.mentions(then) || self.mentions(els) {
                    self.escaped = true;
                }
                self.walk_expr(cond);
            }
            Expr::ListComp { elem, iter, cond, .. } => {
                if self.mentions(elem) {
                    self.escaped = true;
                }
                self.walk_expr(iter);
                if let Some(c) = cond {
                    self.walk_expr(c);
                }
            }
            Expr::FStr(parts) => {
                for p in parts {
                    if let FStrPart::Expr(e) = p {
                        self.walk_expr(e);
                    }
                }
            }
            // Any lambda may capture x; a closure escapes only if it captures x.
            Expr::Lambda { .. } => {
                self.escaped = true;
            }
            Expr::Closure { captures, .. } => {
                if captures.iter().any(|c| self.mentions(c)) {
                    self.escaped = true;
                }
            }
        }
    }
}
