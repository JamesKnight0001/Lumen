//! Whole-program type analysis proving which variables, params, and returns
//! stay pure i64, f64, or list-of-f64, letting backends pick unboxed reps.
//! Soundness: facts start assumed-true and the greatest fixpoint only ever
//! REMOVES them, so a conservative "no" is safe but a wrong "yes" is not. A bogus
//! float-list verdict is worst: a backend would read boxed values as raw f64 bits.

use crate::ast::{BinOp, Expr, FStrPart, Item, Program, Stmt, UnOp};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct IntInfo {
    pub int_vars: HashMap<String, HashSet<String>>,

    pub int_ret: HashSet<String>,

    pub float_vars: HashMap<String, HashSet<String>>,

    pub float_ret: HashSet<String>,

    pub flvars: HashMap<String, HashSet<String>>,

    pub flret: HashSet<String>,

    pub ilvars: HashMap<String, HashSet<String>>,

    pub ilret: HashSet<String>,
}

impl IntInfo {
    pub fn is_ivar(&self, func: &str, name: &str) -> bool {
        self.int_vars.get(func).is_some_and(|s| s.contains(name))
    }
    pub fn is_fvar(&self, func: &str, name: &str) -> bool {
        self.float_vars.get(func).is_some_and(|s| s.contains(name))
    }
    pub fn is_flist(&self, func: &str, name: &str) -> bool {
        self.flvars.get(func).is_some_and(|s| s.contains(name))
    }
    pub fn is_ilist(&self, func: &str, name: &str) -> bool {
        self.ilvars.get(func).is_some_and(|s| s.contains(name))
    }
}

struct FnView<'a> {
    name: String,
    params: Vec<String>,
    body: &'a [Stmt],
}

struct CallSite<'a> {
    callee: String,
    caller: String,
    args: Vec<&'a Expr>,
}

pub fn analyze(prog: &Program) -> IntInfo {
    let mut fns: Vec<FnView> = Vec::new();
    for item in prog {
        if let Item::Fn(f) = item {
            fns.push(FnView {
                name: f.name.clone(),
                params: f.params.iter().map(|p| p.name.clone()).collect(),
                body: &f.body,
            });
        }
    }
    let fn_names: HashSet<String> = fns.iter().map(|f| f.name.clone()).collect();

    // Seed optimistically: assume every param and assigned var is int, then let
    // the fixpoint below remove any that can be disproven.
    let mut int_vars: HashMap<String, HashSet<String>> = HashMap::new();
    for f in &fns {
        let mut vars: HashSet<String> = f.params.iter().cloned().collect();
        seed_vars(f.body, &mut vars);
        int_vars.insert(f.name.clone(), vars);
    }

    // Functions reached via a struct method: method dispatch can pass any boxed
    // value, so their typed-param assumption is unsound and we drop it below.
    let mut method_reached: HashSet<String> = HashSet::new();
    for item in prog {
        if let Item::Struct(s) = item {
            for m in &s.methods {
                fn_refs(&m.body, &fn_names, &mut method_reached);
            }
        }
    }

    let mut int_ret: HashSet<String> = fn_names.clone();

    // Same hazard for escaped functions (used as values/closures): an unseen
    // caller may pass non-int args, so their params lose the int guarantee.
    let escaped = escaped_fns(&fns, &fn_names);
    for fname in escaped.iter().chain(method_reached.iter()) {
        if let Some(set) = int_vars.get_mut(fname) {
            if let Some(f) = fns.iter().find(|f| &f.name == fname) {
                for p in &f.params {
                    set.remove(p);
                }
            }
        }
    }

    let mut calls: Vec<CallSite> = Vec::new();
    for f in &fns {
        collect_calls(f.body, &f.name, &mut calls);
    }
    let params_of: HashMap<&str, &Vec<String>> =
        fns.iter().map(|f| (f.name.as_str(), &f.params)).collect();

    // Int-list candidates co-evolve WITH scalar-int below: an index into a proven
    // int-list (a[i]) is itself an int feeding scalar-int facts. Both are greatest
    // fixpoints (seed optimistic, only ever remove), so a shared loop converges
    // soundly. Seeded here so the first round can see them.
    let mut ilvars: HashMap<String, HashSet<String>> = HashMap::new();
    for f in &fns {
        let mut vars: HashSet<String> = f.params.iter().cloned().collect();
        seed_vars(f.body, &mut vars);
        ilvars.insert(f.name.clone(), vars);
    }
    for fname in escaped.iter().chain(method_reached.iter()) {
        if let Some(set) = ilvars.get_mut(fname) {
            if let Some(f) = fns.iter().find(|f| &f.name == fname) {
                for p in &f.params {
                    set.remove(p);
                }
            }
        }
    }
    let mut ilret: HashSet<String> = fn_names.clone();
    // Shared per-function use facts (bad_use / pushes / index_stores). Element-type
    // agnostic, so int-list and float-list both read them.
    let mut ilist_facts: HashMap<String, FlistFacts> = HashMap::new();
    for f in &fns {
        ilist_facts.insert(f.name.clone(), flist_collect(f.body));
    }

    // Int fixpoint: shrink until nothing changes. Snapshots taken at the top so
    // every test reads a consistent state within the round.
    loop {
        let mut changed = false;
        let vsnap = int_vars.clone();
        let rsnap = int_ret.clone();
        let ilsnap = ilvars.clone();

        for f in &fns {
            let mut assigns: Vec<(String, ValSrc)> = Vec::new();
            assignments(f.body, &mut assigns);
            let cur = int_vars.get_mut(&f.name).unwrap();
            // A var stays int only if EVERY assignment to it is provably int.
            let drop: Vec<String> = cur
                .iter()
                .filter(|v| {
                    !assigns
                        .iter()
                        .filter(|(name, _)| &name == v)
                        .all(|(_, src)| val_int(src, &f.name, &vsnap, &rsnap, &fn_names, &ilsnap))
                })
                .cloned()
                .collect();
            for v in drop {
                cur.remove(&v);
                changed = true;
            }
        }

        // Interprocedural: a callee param can only stay int if every call passes
        // a provably int argument. Drop the param at the callee on any bad arg.
        for cs in &calls {
            let Some(params) = params_of.get(cs.callee.as_str()) else {
                continue;
            };
            for (k, pname) in params.iter().enumerate() {
                if !int_vars
                    .get(&cs.callee)
                    .map(|s| s.contains(pname))
                    .unwrap_or(false)
                {
                    continue;
                }
                let arg_ok = cs
                    .args
                    .get(k)
                    .map(|a| is_iexpr(a, &cs.caller, &vsnap, &rsnap, &fn_names, &ilsnap))
                    .unwrap_or(false);
                if !arg_ok {
                    int_vars.get_mut(&cs.callee).unwrap().remove(pname);
                    changed = true;
                }
            }
        }

        // Int-list verdict, co-evolving here. A var keeps it only if it never escapes
        // (bad_use) and every write (assign/push/index-store) is provably int. Reads
        // via a[i] feed scalar-int above through ilsnap, closing the cycle. A wrong
        // "yes" lets the backend read boxed words as raw i64, as unsound as float-list.
        for f in &fns {
            let facts = ilist_facts.get(&f.name).unwrap();
            let mut assigns: Vec<(String, ValSrc)> = Vec::new();
            assignments(f.body, &mut assigns);
            let cur = ilvars.get(&f.name).unwrap().clone();
            let mut to_drop: Vec<String> = Vec::new();
            for v in &cur {
                if facts.bad_use.contains(v) {
                    to_drop.push(v.clone());
                    continue;
                }
                let assigns_ok = assigns
                    .iter()
                    .filter(|(name, _)| name == v)
                    .all(|(_, src)| match src {
                        ValSrc::Expr(e) => ilist_ok(e, &f.name, &vsnap, &rsnap, &fn_names, &ilsnap),
                        // A `for x in lo..hi` loop var is an int scalar, not a
                        // list: IntRange builds the loop variable, never the list.
                        ValSrc::IntRange | ValSrc::NonInt => false,
                    });
                if !assigns_ok {
                    to_drop.push(v.clone());
                    continue;
                }
                let pushes_ok = facts
                    .pushes
                    .iter()
                    .filter(|(name, _)| name == v)
                    .all(|(_, arg)| is_iexpr(arg, &f.name, &vsnap, &rsnap, &fn_names, &ilsnap));
                if !pushes_ok {
                    to_drop.push(v.clone());
                    continue;
                }
                let stores_ok = facts
                    .index_stores
                    .iter()
                    .filter(|(name, _)| name == v)
                    .all(|(_, val)| is_iexpr(val, &f.name, &vsnap, &rsnap, &fn_names, &ilsnap));
                if !stores_ok {
                    to_drop.push(v.clone());
                    continue;
                }
            }
            if !to_drop.is_empty() {
                let m = ilvars.get_mut(&f.name).unwrap();
                for v in to_drop {
                    m.remove(&v);
                    changed = true;
                }
            }
        }

        for cs in &calls {
            let Some(params) = params_of.get(cs.callee.as_str()) else {
                continue;
            };
            for (k, pname) in params.iter().enumerate() {
                if !ilvars
                    .get(&cs.callee)
                    .map(|s| s.contains(pname))
                    .unwrap_or(false)
                {
                    continue;
                }
                let arg_ok = cs
                    .args
                    .get(k)
                    .map(|a| ilist_val(a, &cs.caller, &ilsnap, &ilret, &fn_names))
                    .unwrap_or(false);
                if !arg_ok {
                    ilvars.get_mut(&cs.callee).unwrap().remove(pname);
                    changed = true;
                }
            }
        }

        for f in &fns {
            if ilret.contains(&f.name)
                && !returns_ilist(f.body, &f.name, &ilvars, &ilret, &fn_names)
            {
                ilret.remove(&f.name);
                changed = true;
            }
        }

        for f in &fns {
            if int_ret.contains(&f.name)
                && !all_iret(f.body, &f.name, &vsnap, &rsnap, &fn_names, &ilsnap)
            {
                int_ret.remove(&f.name);
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }

    // Float analysis mirrors the int one. A var already proven int cannot also be
    // float, so exclude the int set from the float seed to keep the domains disjoint.
    let mut float_vars: HashMap<String, HashSet<String>> = HashMap::new();
    for f in &fns {
        let mut vars: HashSet<String> = f.params.iter().cloned().collect();
        seed_vars(f.body, &mut vars);
        if let Some(iv) = int_vars.get(&f.name) {
            vars.retain(|v| !iv.contains(v));
        }
        float_vars.insert(f.name.clone(), vars);
    }
    for fname in escaped.iter().chain(method_reached.iter()) {
        if let Some(set) = float_vars.get_mut(fname) {
            if let Some(f) = fns.iter().find(|f| &f.name == fname) {
                for p in &f.params {
                    set.remove(p);
                }
            }
        }
    }
    let mut float_ret: HashSet<String> = fn_names.clone();

    let mut flvars: HashMap<String, HashSet<String>> = HashMap::new();
    for f in &fns {
        let mut vars: HashSet<String> = f.params.iter().cloned().collect();
        seed_vars(f.body, &mut vars);
        flvars.insert(f.name.clone(), vars);
    }

    for fname in escaped.iter().chain(method_reached.iter()) {
        if let Some(set) = flvars.get_mut(fname) {
            if let Some(f) = fns.iter().find(|f| &f.name == fname) {
                for p in &f.params {
                    set.remove(p);
                }
            }
        }
    }
    let mut flret: HashSet<String> = fn_names.clone();

    // Per-function float-list facts (any unsafe use, every push arg, every
    // indexed-store value). Drives the float-list verdict below.
    let mut flist_facts: HashMap<String, FlistFacts> = HashMap::new();
    for f in &fns {
        flist_facts.insert(f.name.clone(), flist_collect(f.body));
    }

    loop {
        let mut changed = false;
        let vsnap = float_vars.clone();
        let rsnap = float_ret.clone();
        let lsnap = flvars.clone();

        for f in &fns {
            let mut assigns: Vec<(String, ValSrc)> = Vec::new();
            assignments(f.body, &mut assigns);
            let cur = float_vars.get_mut(&f.name).unwrap();
            let drop: Vec<String> = cur
                .iter()
                .filter(|v| {
                    !assigns
                        .iter()
                        .filter(|(name, _)| &name == v)
                        .all(|(_, src)| val_float(src, &f.name, &vsnap, &rsnap, &fn_names, &lsnap))
                })
                .cloned()
                .collect();
            for v in drop {
                cur.remove(&v);
                changed = true;
            }
        }

        for cs in &calls {
            let Some(params) = params_of.get(cs.callee.as_str()) else {
                continue;
            };
            for (k, pname) in params.iter().enumerate() {
                if !float_vars
                    .get(&cs.callee)
                    .map(|s| s.contains(pname))
                    .unwrap_or(false)
                {
                    continue;
                }
                let arg_ok = cs
                    .args
                    .get(k)
                    .map(|a| is_fexpr(a, &cs.caller, &vsnap, &rsnap, &fn_names, &lsnap))
                    .unwrap_or(false);
                if !arg_ok {
                    float_vars.get_mut(&cs.callee).unwrap().remove(pname);
                    changed = true;
                }
            }
        }

        for f in &fns {
            if float_ret.contains(&f.name)
                && !all_fret(f.body, &f.name, &vsnap, &rsnap, &fn_names, &lsnap)
            {
                float_ret.remove(&f.name);
                changed = true;
            }
        }

        // Float-list is the strict one: a var keeps the verdict only if it never
        // leaks a non-float element and every write (assign, push, indexed store)
        // is provably a float. A wrong "yes" lets a backend read boxed values as
        // raw f64 bits.
        for f in &fns {
            let facts = flist_facts.get(&f.name).unwrap();
            let mut assigns: Vec<(String, ValSrc)> = Vec::new();
            assignments(f.body, &mut assigns);
            let cur = flvars.get(&f.name).unwrap().clone();
            let mut to_drop: Vec<String> = Vec::new();
            for v in &cur {
                // Any non-allowlisted use (escape, mixed op, etc.) disqualifies it.
                if facts.bad_use.contains(v) {
                    to_drop.push(v.clone());
                    continue;
                }

                let assigns_ok = assigns
                    .iter()
                    .filter(|(name, _)| name == v)
                    .all(|(_, src)| match src {
                        ValSrc::Expr(e) => flist_ok(e, &f.name, &vsnap, &rsnap, &fn_names, &lsnap),

                        ValSrc::IntRange | ValSrc::NonInt => false,
                    });
                if !assigns_ok {
                    to_drop.push(v.clone());
                    continue;
                }

                let pushes_ok = facts
                    .pushes
                    .iter()
                    .filter(|(name, _)| name == v)
                    .all(|(_, arg)| is_fexpr(arg, &f.name, &vsnap, &rsnap, &fn_names, &lsnap));
                if !pushes_ok {
                    to_drop.push(v.clone());
                    continue;
                }

                let stores_ok = facts
                    .index_stores
                    .iter()
                    .filter(|(name, _)| name == v)
                    .all(|(_, val)| is_fexpr(val, &f.name, &vsnap, &rsnap, &fn_names, &lsnap));
                if !stores_ok {
                    to_drop.push(v.clone());
                    continue;
                }
            }
            if !to_drop.is_empty() {
                let m = flvars.get_mut(&f.name).unwrap();
                for v in to_drop {
                    m.remove(&v);
                    changed = true;
                }
            }
        }

        for cs in &calls {
            let Some(params) = params_of.get(cs.callee.as_str()) else {
                continue;
            };
            for (k, pname) in params.iter().enumerate() {
                if !flvars
                    .get(&cs.callee)
                    .map(|s| s.contains(pname))
                    .unwrap_or(false)
                {
                    continue;
                }
                let arg_ok = cs
                    .args
                    .get(k)
                    .map(|a| flist_val(a, &cs.caller, &lsnap, &flret, &fn_names))
                    .unwrap_or(false);
                if !arg_ok {
                    flvars.get_mut(&cs.callee).unwrap().remove(pname);
                    changed = true;
                }
            }
        }

        for f in &fns {
            if flret.contains(&f.name)
                && !returns_flist(f.body, &f.name, &flvars, &flret, &fn_names)
            {
                flret.remove(&f.name);
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }

    // An int-LIST var is not a scalar int, yet the int fixpoint may leave a
    // never-assigned list param in int_vars (params seed optimistically; only
    // assignments disqualify). Strip them so codegen does not treat the list
    // handle as a raw scalar when loading it for a[i].
    for (fname, lvars) in &ilvars {
        if let Some(ivars) = int_vars.get_mut(fname) {
            for v in lvars {
                ivars.remove(v);
            }
        }
    }

    IntInfo {
        int_vars,
        int_ret,
        float_vars,
        float_ret,
        flvars,
        flret,
        ilvars,
        ilret,
    }
}

enum ValSrc<'a> {
    Expr(&'a Expr),

    IntRange,

    NonInt,
}

fn val_int(
    src: &ValSrc,
    func: &str,
    vars: &HashMap<String, HashSet<String>>,
    ret: &HashSet<String>,
    fns: &HashSet<String>,
    ilist: &HashMap<String, HashSet<String>>,
) -> bool {
    match src {
        ValSrc::IntRange => true,
        ValSrc::NonInt => false,
        ValSrc::Expr(e) => is_iexpr(e, func, vars, ret, fns, ilist),
    }
}

// is_iexpr with int-list awareness: a[i] on a proven int-list var is an int.
// The int-list verdict co-evolves with scalar-int in the same fixpoint, so
// `ilist` is the in-progress ilvars snapshot. Conservative on every other shape.
fn is_iexpr(
    e: &Expr,
    func: &str,
    vars: &HashMap<String, HashSet<String>>,
    ret: &HashSet<String>,
    fns: &HashSet<String>,
    ilist: &HashMap<String, HashSet<String>>,
) -> bool {
    match e {
        Expr::Int(_) => true,
        Expr::Ident(n) => vars.get(func).is_some_and(|s| s.contains(n)),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
        } => is_iexpr(expr, func, vars, ret, fns, ilist),
        Expr::Binary { op, lhs, rhs } => {
            int_arith(*op)
                && is_iexpr(lhs, func, vars, ret, fns, ilist)
                && is_iexpr(rhs, func, vars, ret, fns, ilist)
        }
        Expr::Call { callee, .. } => {
            matches!(&**callee, Expr::Ident(n) if fns.contains(n) && ret.contains(n))
        }
        Expr::Index { obj, index } => {
            matches!(&**obj, Expr::Ident(n) if ilist.get(func).is_some_and(|s| s.contains(n)))
                && idx_intish(index, func, vars, ret, fns)
        }
        _ => false,
    }
}

fn int_arith(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
    )
}

fn all_iret(
    body: &[Stmt],
    func: &str,
    vars: &HashMap<String, HashSet<String>>,
    ret: &HashSet<String>,
    fns: &HashSet<String>,
    ilist: &HashMap<String, HashSet<String>>,
) -> bool {
    let mut any = false;
    let mut ok = true;
    walk_returns(body, &mut |e: &Option<Expr>| {
        any = true;
        match e {
            Some(val) => {
                if !is_iexpr(val, func, vars, ret, fns, ilist) {
                    ok = false;
                }
            }
            None => ok = false,
        }
    });
    ok && any && matches!(body.last(), Some(Stmt::Return(_)))
}

// flist_bad marks a var as "used in a way that may leak a non-float element":
// any appearance outside the safe positions (indexing, len, push) poisons it.
fn val_float(
    src: &ValSrc,
    func: &str,
    vars: &HashMap<String, HashSet<String>>,
    ret: &HashSet<String>,
    fns: &HashSet<String>,
    flist: &HashMap<String, HashSet<String>>,
) -> bool {
    match src {
        ValSrc::IntRange | ValSrc::NonInt => false,
        ValSrc::Expr(e) => is_fexpr(e, func, vars, ret, fns, flist),
    }
}

fn is_fexpr(
    e: &Expr,
    func: &str,
    vars: &HashMap<String, HashSet<String>>,
    ret: &HashSet<String>,
    fns: &HashSet<String>,
    flist: &HashMap<String, HashSet<String>>,
) -> bool {
    match e {
        Expr::Float(_) => true,
        Expr::Ident(n) => vars.get(func).is_some_and(|s| s.contains(n)),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
        } => is_fexpr(expr, func, vars, ret, fns, flist),
        Expr::Binary { op, lhs, rhs } => {
            float_arith(*op)
                && is_fexpr(lhs, func, vars, ret, fns, flist)
                && is_fexpr(rhs, func, vars, ret, fns, flist)
        }
        Expr::Call { callee, .. } => {
            matches!(&**callee, Expr::Ident(n) if fns.contains(n) && ret.contains(n))
        }

        Expr::Index { obj, index } => {
            matches!(&**obj, Expr::Ident(n) if flist.get(func).is_some_and(|s| s.contains(n)))
                && idx_intish(index, func, vars, ret, fns)
        }
        _ => false,
    }
}

fn idx_intish(
    e: &Expr,
    func: &str,
    ivars: &HashMap<String, HashSet<String>>,
    iret: &HashSet<String>,
    fns: &HashSet<String>,
) -> bool {
    // The index value itself can be any int-shaped expression; the element type
    // is what matters and that is enforced where the list is built, not here.
    let _ = (func, ivars, iret, fns);
    matches!(
        e,
        Expr::Int(_)
            | Expr::Ident(_)
            | Expr::Binary { .. }
            | Expr::Unary { .. }
            | Expr::Call { .. }
    )
}

fn float_arith(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
    )
}

#[derive(Default)]
struct FlistFacts {
    bad_use: HashSet<String>,

    pushes: Vec<(String, Expr)>,

    index_stores: Vec<(String, Expr)>,
}

fn flist_collect(body: &[Stmt]) -> FlistFacts {
    let mut facts = FlistFacts::default();
    for st in body {
        flist_stmt(st, &mut facts);
    }
    facts
}

fn flist_bad(e: &Expr, facts: &mut FlistFacts) {
    match e {
        Expr::Ident(n) => {
            facts.bad_use.insert(n.clone());
        }
        Expr::Unary { expr, .. } => flist_bad(expr, facts),
        Expr::Binary { lhs, rhs, .. } => {
            flist_bad(lhs, facts);
            flist_bad(rhs, facts);
        }
        Expr::Call { callee, args } => {
            flist_bad(callee, facts);
            for a in args {
                flist_bad(a, facts);
            }
        }
        Expr::NamedCall { callee, args } => {
            flist_bad(callee, facts);
            for (_, a) in args {
                flist_bad(a, facts);
            }
        }
        Expr::Method { obj, args, .. } => {
            flist_bad(obj, facts);
            for a in args {
                flist_bad(a, facts);
            }
        }
        Expr::Field { obj, .. } => flist_bad(obj, facts),
        Expr::Index { obj, index } => {
            if !matches!(&**obj, Expr::Ident(_)) {
                flist_bad(obj, facts);
            }
            flist_bad(index, facts);
        }
        Expr::Slice { obj, lo, hi } => {
            flist_bad(obj, facts);
            if let Some(lo) = lo {
                flist_bad(lo, facts);
            }
            if let Some(hi) = hi {
                flist_bad(hi, facts);
            }
        }
        Expr::List(xs) => {
            for x in xs {
                flist_bad(x, facts);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs {
                flist_bad(k, facts);
                flist_bad(v, facts);
            }
        }
        Expr::Range { lo, hi } => {
            flist_bad(lo, facts);
            flist_bad(hi, facts);
        }
        Expr::IfElse { cond, then, els } => {
            flist_bad(cond, facts);
            flist_bad(then, facts);
            flist_bad(els, facts);
        }
        Expr::ListComp {
            elem, iter, cond, ..
        } => {
            flist_bad(elem, facts);
            flist_bad(iter, facts);
            if let Some(c) = cond {
                flist_bad(c, facts);
            }
        }
        Expr::Closure { captures, .. } => {
            for c in captures {
                flist_bad(c, facts);
            }
        }
        Expr::FStr(parts) => {
            for p in parts {
                if let FStrPart::Expr(pe) = p {
                    flist_bad(pe, facts);
                }
            }
        }
        Expr::Lambda { .. } => {}
        _ => {}
    }
}

fn flist_visit(e: &Expr, facts: &mut FlistFacts) {
    match e {
        Expr::Index { obj, index } => {
            if let Expr::Ident(_) = &**obj {
                flist_visit(index, facts);
            } else {
                flist_visit(obj, facts);
                flist_visit(index, facts);
            }
        }

        Expr::Method { obj, name, args } => {
            // Only len/push on a bare identifier are safe receivers. push args are
            // recorded so the verdict loop can require floats; anything else poisons it.
            let receiver_safe =
                matches!(&**obj, Expr::Ident(_)) && (name == "len" || name == "push");
            if receiver_safe {
                if name == "push" {
                    if let Expr::Ident(n) = &**obj {
                        for a in args {
                            facts.pushes.push((n.clone(), a.clone()));
                        }
                    }
                }

                for a in args {
                    flist_visit(a, facts);
                }
            } else {
                flist_bad(obj, facts);
                for a in args {
                    flist_bad(a, facts);
                }
            }
        }

        Expr::Ident(n) => {
            facts.bad_use.insert(n.clone());
        }

        Expr::Unary { expr, .. } => flist_visit(expr, facts),
        Expr::Binary { lhs, rhs, .. } => {
            flist_visit(lhs, facts);
            flist_visit(rhs, facts);
        }
        Expr::Call { callee, args } => {
            flist_bad(callee, facts);
            for a in args {
                flist_bad(a, facts);
            }
        }
        Expr::NamedCall { callee, args } => {
            flist_bad(callee, facts);
            for (_, a) in args {
                flist_bad(a, facts);
            }
        }
        Expr::Field { obj, .. } => flist_bad(obj, facts),
        Expr::Slice { obj, lo, hi } => {
            flist_bad(obj, facts);
            if let Some(lo) = lo {
                flist_bad(lo, facts);
            }
            if let Some(hi) = hi {
                flist_bad(hi, facts);
            }
        }
        Expr::List(xs) => {
            for x in xs {
                flist_bad(x, facts);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs {
                flist_bad(k, facts);
                flist_bad(v, facts);
            }
        }
        Expr::Range { lo, hi } => {
            flist_bad(lo, facts);
            flist_bad(hi, facts);
        }
        Expr::IfElse { cond, then, els } => {
            flist_bad(cond, facts);
            flist_bad(then, facts);
            flist_bad(els, facts);
        }
        Expr::ListComp {
            elem, iter, cond, ..
        } => {
            flist_bad(elem, facts);
            flist_bad(iter, facts);
            if let Some(c) = cond {
                flist_bad(c, facts);
            }
        }
        Expr::Closure { captures, .. } => {
            for c in captures {
                flist_bad(c, facts);
            }
        }
        Expr::FStr(parts) => {
            for p in parts {
                if let FStrPart::Expr(pe) = p {
                    flist_bad(pe, facts);
                }
            }
        }
        _ => {}
    }
}

fn flist_stmt(st: &Stmt, facts: &mut FlistFacts) {
    match st {
        Stmt::Let { value, .. } => flist_visit(value, facts),
        Stmt::Assign { target, value } => match target {
            Expr::Index { obj, index } => {
                if let Expr::Ident(n) = &**obj {
                    facts.index_stores.push((n.clone(), value.clone()));
                    flist_visit(index, facts);
                } else {
                    flist_visit(obj, facts);
                    flist_visit(index, facts);
                }
                flist_visit(value, facts);
            }

            Expr::Ident(_) => flist_visit(value, facts),

            other => {
                flist_visit(other, facts);
                flist_visit(value, facts);
            }
        },
        Stmt::ExprStmt(e) => flist_visit(e, facts),
        Stmt::Return(Some(e)) => {
            // Returning a value lets it escape this function under an unknown
            // type, so any var in the returned expression is poisoned.
            flist_bad(e, facts);
        }
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            flist_visit(cond, facts);
            for s in then {
                flist_stmt(s, facts);
            }
            for (c, b) in elifs {
                flist_visit(c, facts);
                for s in b {
                    flist_stmt(s, facts);
                }
            }
            if let Some(b) = els {
                for s in b {
                    flist_stmt(s, facts);
                }
            }
        }
        Stmt::While { cond, body } => {
            flist_visit(cond, facts);
            for s in body {
                flist_stmt(s, facts);
            }
        }
        Stmt::For { iter, body, .. } => {
            flist_visit(iter, facts);
            for s in body {
                flist_stmt(s, facts);
            }
        }
        Stmt::Try {
            body, catch_body, ..
        } => {
            for s in body {
                flist_stmt(s, facts);
            }
            for s in catch_body {
                flist_stmt(s, facts);
            }
        }
        Stmt::Raise(e) => flist_visit(e, facts),
        _ => {}
    }
}

fn flist_ok(
    e: &Expr,
    func: &str,
    fvars: &HashMap<String, HashSet<String>>,
    fret: &HashSet<String>,
    fns: &HashSet<String>,
    flist: &HashMap<String, HashSet<String>>,
) -> bool {
    match e {
        Expr::List(elems) => elems
            .iter()
            .all(|el| is_fexpr(el, func, fvars, fret, fns, flist)),
        Expr::ListComp { elem, var, .. } => {
            let _ = var;
            is_fexpr(elem, func, fvars, fret, fns, flist)
        }
        Expr::Ident(n) => flist.get(func).is_some_and(|s| s.contains(n)),
        Expr::Call { callee, .. } => {
            matches!(&**callee, Expr::Ident(n) if fns.contains(n) && fret.contains(n))
        }
        _ => false,
    }
}

// Int analogue of flist_ok: an assignment RHS that produces a valid int-list.
fn ilist_ok(
    e: &Expr,
    func: &str,
    ivars: &HashMap<String, HashSet<String>>,
    iret: &HashSet<String>,
    fns: &HashSet<String>,
    ilist: &HashMap<String, HashSet<String>>,
) -> bool {
    match e {
        Expr::List(elems) => elems
            .iter()
            .all(|el| is_iexpr(el, func, ivars, iret, fns, ilist)),
        Expr::ListComp { elem, var, .. } => {
            let _ = var;
            is_iexpr(elem, func, ivars, iret, fns, ilist)
        }
        Expr::Ident(n) => ilist.get(func).is_some_and(|s| s.contains(n)),
        Expr::Call { callee, .. } => {
            matches!(&**callee, Expr::Ident(n) if fns.contains(n) && iret.contains(n))
        }
        // A bare integer range `lo..hi` builds an int list.
        Expr::Range { .. } => true,
        _ => false,
    }
}

fn flist_val(
    e: &Expr,
    caller: &str,
    flist: &HashMap<String, HashSet<String>>,
    flist_ret: &HashSet<String>,
    fns: &HashSet<String>,
) -> bool {
    match e {
        Expr::Ident(n) => flist.get(caller).is_some_and(|s| s.contains(n)),
        Expr::Call { callee, .. } => {
            matches!(&**callee, Expr::Ident(n) if fns.contains(n) && flist_ret.contains(n))
        }
        _ => false,
    }
}

fn returns_flist(
    body: &[Stmt],
    func: &str,
    flist: &HashMap<String, HashSet<String>>,
    flist_ret: &HashSet<String>,
    fns: &HashSet<String>,
) -> bool {
    let mut any = false;
    let mut ok = true;
    walk_returns(body, &mut |e: &Option<Expr>| {
        any = true;
        match e {
            Some(val) => {
                if !flist_val(val, func, flist, flist_ret, fns) {
                    ok = false;
                }
            }
            None => ok = false,
        }
    });
    ok && any && matches!(body.last(), Some(Stmt::Return(_)))
}

// Int-list analogues of flist_val / returns_flist. Element type is enforced at
// the writes (is_iexpr); here we only track expressions yielding a proven int-list.
fn ilist_val(
    e: &Expr,
    caller: &str,
    ilist: &HashMap<String, HashSet<String>>,
    ilist_ret: &HashSet<String>,
    fns: &HashSet<String>,
) -> bool {
    match e {
        Expr::Ident(n) => ilist.get(caller).is_some_and(|s| s.contains(n)),
        Expr::Call { callee, .. } => {
            matches!(&**callee, Expr::Ident(n) if fns.contains(n) && ilist_ret.contains(n))
        }
        _ => false,
    }
}

fn returns_ilist(
    body: &[Stmt],
    func: &str,
    ilist: &HashMap<String, HashSet<String>>,
    ilist_ret: &HashSet<String>,
    fns: &HashSet<String>,
) -> bool {
    let mut any = false;
    let mut ok = true;
    walk_returns(body, &mut |e: &Option<Expr>| {
        any = true;
        match e {
            Some(val) => {
                if !ilist_val(val, func, ilist, ilist_ret, fns) {
                    ok = false;
                }
            }
            None => ok = false,
        }
    });
    ok && any && matches!(body.last(), Some(Stmt::Return(_)))
}

fn all_fret(
    body: &[Stmt],
    func: &str,
    vars: &HashMap<String, HashSet<String>>,
    ret: &HashSet<String>,
    fns: &HashSet<String>,
    flist: &HashMap<String, HashSet<String>>,
) -> bool {
    let mut any = false;
    let mut ok = true;
    walk_returns(body, &mut |e: &Option<Expr>| {
        any = true;
        match e {
            Some(val) => {
                if !is_fexpr(val, func, vars, ret, fns, flist) {
                    ok = false;
                }
            }
            None => ok = false,
        }
    });
    ok && any && matches!(body.last(), Some(Stmt::Return(_)))
}

fn walk_returns(body: &[Stmt], f: &mut dyn FnMut(&Option<Expr>)) {
    for st in body {
        match st {
            Stmt::Return(e) => f(e),
            Stmt::If {
                then, elifs, els, ..
            } => {
                walk_returns(then, f);
                for (_, b) in elifs {
                    walk_returns(b, f);
                }
                if let Some(b) = els {
                    walk_returns(b, f);
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } => walk_returns(body, f),
            Stmt::Try {
                body, catch_body, ..
            } => {
                walk_returns(body, f);
                walk_returns(catch_body, f);
            }
            _ => {}
        }
    }
}

fn seed_vars(body: &[Stmt], out: &mut HashSet<String>) {
    for st in body {
        match st {
            Stmt::Let { name, .. } => {
                out.insert(name.clone());
            }
            Stmt::Assign {
                target: Expr::Ident(n),
                ..
            } => {
                out.insert(n.clone());
            }
            Stmt::If {
                then, elifs, els, ..
            } => {
                seed_vars(then, out);
                for (_, b) in elifs {
                    seed_vars(b, out);
                }
                if let Some(b) = els {
                    seed_vars(b, out);
                }
            }
            Stmt::While { body, .. } => seed_vars(body, out),
            Stmt::For { var, body, .. } => {
                out.insert(var.clone());
                seed_vars(body, out);
            }
            Stmt::Try {
                body,
                catch_var,
                catch_body,
            } => {
                seed_vars(body, out);
                out.insert(catch_var.clone());
                seed_vars(catch_body, out);
            }
            _ => {}
        }
    }
}

fn assignments<'a>(body: &'a [Stmt], out: &mut Vec<(String, ValSrc<'a>)>) {
    for st in body {
        match st {
            Stmt::Let { name, value, .. } => out.push((name.clone(), ValSrc::Expr(value))),
            Stmt::Assign {
                target: Expr::Ident(n),
                value,
            } => out.push((n.clone(), ValSrc::Expr(value))),
            Stmt::If {
                then, elifs, els, ..
            } => {
                assignments(then, out);
                for (_, b) in elifs {
                    assignments(b, out);
                }
                if let Some(b) = els {
                    assignments(b, out);
                }
            }
            Stmt::While { body, .. } => assignments(body, out),
            Stmt::For { var, iter, body } => {
                let int_elems = matches!(iter, Expr::Range { .. })
                    || matches!(iter, Expr::Call { callee, .. }
                        if matches!(&**callee, Expr::Ident(n) if n == "range"));
                out.push((
                    var.clone(),
                    if int_elems {
                        ValSrc::IntRange
                    } else {
                        ValSrc::NonInt
                    },
                ));
                assignments(body, out);
            }
            Stmt::Try {
                body,
                catch_var,
                catch_body,
            } => {
                assignments(body, out);

                out.push((catch_var.clone(), ValSrc::NonInt));
                assignments(catch_body, out);
            }
            _ => {}
        }
    }
}

// Functions whose name is used as a value (arg, closure capture) not directly
// called: callable with unseen args, so their typed-param assumptions are unsound.
fn escaped_fns(fns: &[FnView], fn_names: &HashSet<String>) -> HashSet<String> {
    let mut escaped: HashSet<String> = HashSet::new();
    fn ve(e: &Expr, fn_names: &HashSet<String>, out: &mut HashSet<String>) {
        match e {
            Expr::Ident(n) => {
                if fn_names.contains(n) {
                    out.insert(n.clone());
                }
            }
            Expr::Call { callee, args } => {
                if !matches!(&**callee, Expr::Ident(_)) {
                    ve(callee, fn_names, out);
                }
                for a in args {
                    ve(a, fn_names, out);
                }
            }
            Expr::Closure { fn_name, captures } => {
                if fn_names.contains(fn_name) {
                    out.insert(fn_name.clone());
                }
                for c in captures {
                    ve(c, fn_names, out);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                ve(lhs, fn_names, out);
                ve(rhs, fn_names, out);
            }
            Expr::Unary { expr, .. } => ve(expr, fn_names, out),
            Expr::IfElse { cond, then, els } => {
                ve(cond, fn_names, out);
                ve(then, fn_names, out);
                ve(els, fn_names, out);
            }
            Expr::ListComp {
                elem, iter, cond, ..
            } => {
                ve(elem, fn_names, out);
                ve(iter, fn_names, out);
                if let Some(c) = cond {
                    ve(c, fn_names, out);
                }
            }
            Expr::Method { obj, args, .. } => {
                ve(obj, fn_names, out);
                for a in args {
                    ve(a, fn_names, out);
                }
            }
            Expr::Index { obj, index } => {
                ve(obj, fn_names, out);
                ve(index, fn_names, out);
            }
            Expr::List(xs) => {
                for x in xs {
                    ve(x, fn_names, out);
                }
            }
            Expr::NamedCall { callee, args } => {
                ve(callee, fn_names, out);
                for (_, a) in args {
                    ve(a, fn_names, out);
                }
            }
            Expr::Field { obj, .. } => ve(obj, fn_names, out),
            Expr::Range { lo, hi } => {
                ve(lo, fn_names, out);
                ve(hi, fn_names, out);
            }
            Expr::Slice { obj, lo, hi } => {
                ve(obj, fn_names, out);
                if let Some(lo) = lo {
                    ve(lo, fn_names, out);
                }
                if let Some(hi) = hi {
                    ve(hi, fn_names, out);
                }
            }
            Expr::Map(kvs) => {
                for (k, v) in kvs {
                    ve(k, fn_names, out);
                    ve(v, fn_names, out);
                }
            }
            Expr::FStr(parts) => {
                for p in parts {
                    if let FStrPart::Expr(pe) = p {
                        ve(pe, fn_names, out);
                    }
                }
            }
            _ => {}
        }
    }
    fn vs(st: &Stmt, fn_names: &HashSet<String>, out: &mut HashSet<String>) {
        match st {
            Stmt::Let { value, .. } | Stmt::ExprStmt(value) | Stmt::Return(Some(value)) => {
                ve(value, fn_names, out)
            }
            Stmt::Assign { target, value } => {
                ve(target, fn_names, out);
                ve(value, fn_names, out);
            }
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } => {
                ve(cond, fn_names, out);
                for s in then {
                    vs(s, fn_names, out);
                }
                for (c, b) in elifs {
                    ve(c, fn_names, out);
                    for s in b {
                        vs(s, fn_names, out);
                    }
                }
                if let Some(b) = els {
                    for s in b {
                        vs(s, fn_names, out);
                    }
                }
            }
            Stmt::While { cond, body } => {
                ve(cond, fn_names, out);
                for s in body {
                    vs(s, fn_names, out);
                }
            }
            Stmt::For { iter, body, .. } => {
                ve(iter, fn_names, out);
                for s in body {
                    vs(s, fn_names, out);
                }
            }
            Stmt::Try {
                body, catch_body, ..
            } => {
                for s in body {
                    vs(s, fn_names, out);
                }
                for s in catch_body {
                    vs(s, fn_names, out);
                }
            }
            Stmt::Raise(e) => ve(e, fn_names, out),
            _ => {}
        }
    }
    for f in fns {
        for st in f.body {
            vs(st, fn_names, &mut escaped);
        }
    }
    escaped
}

fn fn_refs(body: &[Stmt], fn_names: &HashSet<String>, out: &mut HashSet<String>) {
    fn ve(e: &Expr, fn_names: &HashSet<String>, out: &mut HashSet<String>) {
        match e {
            Expr::Ident(n) => {
                if fn_names.contains(n) {
                    out.insert(n.clone());
                }
            }
            Expr::Closure { fn_name, captures } => {
                if fn_names.contains(fn_name) {
                    out.insert(fn_name.clone());
                }
                for c in captures {
                    ve(c, fn_names, out);
                }
            }
            Expr::Call { callee, args } => {
                ve(callee, fn_names, out);
                for a in args {
                    ve(a, fn_names, out);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                ve(lhs, fn_names, out);
                ve(rhs, fn_names, out);
            }
            Expr::Unary { expr, .. } => ve(expr, fn_names, out),
            Expr::IfElse { cond, then, els } => {
                ve(cond, fn_names, out);
                ve(then, fn_names, out);
                ve(els, fn_names, out);
            }
            Expr::ListComp {
                elem, iter, cond, ..
            } => {
                ve(elem, fn_names, out);
                ve(iter, fn_names, out);
                if let Some(c) = cond {
                    ve(c, fn_names, out);
                }
            }
            Expr::Method { obj, args, .. } => {
                ve(obj, fn_names, out);
                for a in args {
                    ve(a, fn_names, out);
                }
            }
            Expr::Index { obj, index } => {
                ve(obj, fn_names, out);
                ve(index, fn_names, out);
            }
            Expr::List(xs) => {
                for x in xs {
                    ve(x, fn_names, out);
                }
            }
            Expr::NamedCall { callee, args } => {
                ve(callee, fn_names, out);
                for (_, a) in args {
                    ve(a, fn_names, out);
                }
            }
            Expr::Field { obj, .. } => ve(obj, fn_names, out),
            Expr::Range { lo, hi } => {
                ve(lo, fn_names, out);
                ve(hi, fn_names, out);
            }
            Expr::Slice { obj, lo, hi } => {
                ve(obj, fn_names, out);
                if let Some(lo) = lo {
                    ve(lo, fn_names, out);
                }
                if let Some(hi) = hi {
                    ve(hi, fn_names, out);
                }
            }
            Expr::Map(kvs) => {
                for (k, v) in kvs {
                    ve(k, fn_names, out);
                    ve(v, fn_names, out);
                }
            }
            Expr::FStr(parts) => {
                for p in parts {
                    if let FStrPart::Expr(pe) = p {
                        ve(pe, fn_names, out);
                    }
                }
            }
            _ => {}
        }
    }
    fn vs(st: &Stmt, fn_names: &HashSet<String>, out: &mut HashSet<String>) {
        match st {
            Stmt::Let { value, .. } | Stmt::ExprStmt(value) | Stmt::Return(Some(value)) => {
                ve(value, fn_names, out)
            }
            Stmt::Assign { target, value } => {
                ve(target, fn_names, out);
                ve(value, fn_names, out);
            }
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } => {
                ve(cond, fn_names, out);
                for s in then {
                    vs(s, fn_names, out);
                }
                for (c, b) in elifs {
                    ve(c, fn_names, out);
                    for s in b {
                        vs(s, fn_names, out);
                    }
                }
                if let Some(b) = els {
                    for s in b {
                        vs(s, fn_names, out);
                    }
                }
            }
            Stmt::While { cond, body } => {
                ve(cond, fn_names, out);
                for s in body {
                    vs(s, fn_names, out);
                }
            }
            Stmt::For { iter, body, .. } => {
                ve(iter, fn_names, out);
                for s in body {
                    vs(s, fn_names, out);
                }
            }
            Stmt::Try {
                body, catch_body, ..
            } => {
                for s in body {
                    vs(s, fn_names, out);
                }
                for s in catch_body {
                    vs(s, fn_names, out);
                }
            }
            Stmt::Raise(e) => ve(e, fn_names, out),
            _ => {}
        }
    }
    for st in body {
        vs(st, fn_names, out);
    }
}

fn collect_calls<'a>(body: &'a [Stmt], caller: &str, out: &mut Vec<CallSite<'a>>) {
    fn ve<'a>(e: &'a Expr, caller: &str, out: &mut Vec<CallSite<'a>>) {
        match e {
            Expr::Call { callee, args } => {
                if let Expr::Ident(name) = &**callee {
                    out.push(CallSite {
                        callee: name.clone(),
                        caller: caller.to_string(),
                        args: args.iter().collect(),
                    });
                }
                ve(callee, caller, out);
                for a in args {
                    ve(a, caller, out);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                ve(lhs, caller, out);
                ve(rhs, caller, out);
            }
            Expr::Unary { expr, .. } => ve(expr, caller, out),
            Expr::IfElse { cond, then, els } => {
                ve(cond, caller, out);
                ve(then, caller, out);
                ve(els, caller, out);
            }
            Expr::ListComp {
                elem, iter, cond, ..
            } => {
                ve(elem, caller, out);
                ve(iter, caller, out);
                if let Some(c) = cond {
                    ve(c, caller, out);
                }
            }
            Expr::Method { obj, args, .. } => {
                ve(obj, caller, out);
                for a in args {
                    ve(a, caller, out);
                }
            }
            Expr::Index { obj, index } => {
                ve(obj, caller, out);
                ve(index, caller, out);
            }
            Expr::List(xs) => {
                for x in xs {
                    ve(x, caller, out);
                }
            }
            Expr::NamedCall { callee, args } => {
                ve(callee, caller, out);
                for (_, a) in args {
                    ve(a, caller, out);
                }
            }
            Expr::Field { obj, .. } => ve(obj, caller, out),
            Expr::Range { lo, hi } => {
                ve(lo, caller, out);
                ve(hi, caller, out);
            }
            Expr::Slice { obj, lo, hi } => {
                ve(obj, caller, out);
                if let Some(lo) = lo {
                    ve(lo, caller, out);
                }
                if let Some(hi) = hi {
                    ve(hi, caller, out);
                }
            }
            Expr::Map(kvs) => {
                for (k, v) in kvs {
                    ve(k, caller, out);
                    ve(v, caller, out);
                }
            }
            Expr::FStr(parts) => {
                for p in parts {
                    if let FStrPart::Expr(pe) = p {
                        ve(pe, caller, out);
                    }
                }
            }
            _ => {}
        }
    }
    fn vs<'a>(st: &'a Stmt, caller: &str, out: &mut Vec<CallSite<'a>>) {
        match st {
            Stmt::Let { value, .. } | Stmt::ExprStmt(value) | Stmt::Return(Some(value)) => {
                ve(value, caller, out)
            }
            Stmt::Assign { value, .. } => ve(value, caller, out),
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } => {
                ve(cond, caller, out);
                for s in then {
                    vs(s, caller, out);
                }
                for (c, b) in elifs {
                    ve(c, caller, out);
                    for s in b {
                        vs(s, caller, out);
                    }
                }
                if let Some(b) = els {
                    for s in b {
                        vs(s, caller, out);
                    }
                }
            }
            Stmt::While { cond, body } => {
                ve(cond, caller, out);
                for s in body {
                    vs(s, caller, out);
                }
            }
            Stmt::For { iter, body, .. } => {
                ve(iter, caller, out);
                for s in body {
                    vs(s, caller, out);
                }
            }
            Stmt::Try {
                body, catch_body, ..
            } => {
                for s in body {
                    vs(s, caller, out);
                }
                for s in catch_body {
                    vs(s, caller, out);
                }
            }
            Stmt::Raise(e) => ve(e, caller, out),
            _ => {}
        }
    }
    for st in body {
        vs(st, caller, out);
    }
}

#[cfg(test)]
mod tests {
    use super::analyze;

    fn info(src: &str) -> super::IntInfo {
        let prog = crate::parse_program(src).expect("parse");
        analyze(&prog)
    }

    #[test]
    fn fib_int() {
        let src = "fn fib(n):\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\nfn main():\n    print(fib(10))\n";
        let i = info(src);
        assert!(i.is_ivar("fib", "n"), "n should be proven int");
        assert!(
            i.int_ret.contains("fib"),
            "fib should be proven int-returning"
        );
    }

    #[test]
    fn mixed_notint() {
        let src =
            "fn dbl(x):\n    return x + x\nfn main():\n    print(dbl(1))\n    print(dbl(1.5))\n";
        let i = info(src);
        assert!(
            !i.is_ivar("dbl", "x"),
            "x must not be typed int (float arg)"
        );
    }

    #[test]
    fn reasn_notint() {
        let src = "fn main():\n    let v = 10\n    v = 2.5\n    print(v)\n";
        let i = info(src);
        assert!(
            !i.is_ivar("main", "v"),
            "v reassigned to float must not be int"
        );
    }

    #[test]
    fn pure_ilocal() {
        let src = "fn main():\n    let a = 5\n    let b = a + 3\n    print(b)\n";
        let i = info(src);
        assert!(i.is_ivar("main", "a"));
        assert!(i.is_ivar("main", "b"));
    }

    #[test]
    fn comp_notint() {
        let src = "fn dbl(x):\n    return x * 2\nfn main():\n    print([dbl(c) for c in \"ab\"])\n";
        let i = info(src);
        assert!(
            !i.is_ivar("dbl", "x"),
            "x must not be int: only call site is dbl(c) over a string"
        );
    }

    fn info_lifted(src: &str) -> super::IntInfo {
        let dir = std::env::temp_dir();
        let prog = crate::compile(src, &dir, true).expect("compile");
        analyze(&prog)
    }

    #[test]
    fn esc_notint() {
        let src = "fn inc(x):\n    return x + 1\nfn apply(f, v):\n    return f(v)\nfn main():\n    print(apply(inc, 5))\n";
        let i = info_lifted(src);
        assert!(
            !i.is_ivar("inc", "x"),
            "inc escapes as a value -> its param must not be proven int"
        );
    }

    #[test]
    fn cap_notint() {
        let src = "fn make_rng(seed):\n    let state = seed\n    return fn():\n        state = state + 1\n        return state\nfn main():\n    let r = make_rng(0)\n    print(r())\n";
        let i = info_lifted(src);
        for (fname, vars) in &i.int_vars {
            if fname.starts_with("__lambda") {
                assert!(
                    !vars.contains("state"),
                    "captured cell param `state` must not be proven int in {fname}"
                );
            }
        }
    }

    #[test]
    fn meth_notint() {
        let src = "fn helper(x):\n    return x + 1\nstruct S:\n    v: int\nimpl S:\n    fn go(self):\n        return helper(self.v)\nfn main():\n    let s = S(v: 3)\n    print(s.go())\n";
        let i = info_lifted(src);
        assert!(
            !i.is_ivar("helper", "x"),
            "helper is reached only from a method body -> param must not be int"
        );
    }
}
