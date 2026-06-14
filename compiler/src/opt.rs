//! AST optimizer for Lumen: small-function inlining, constant folding, dead-code
//! elimination, dead-SrcLine stripping, and a conservative common-subexpression
//! pass. Every transform must be semantics-preserving and produce the same
//! observable result under both backends, so folding mirrors interp's exact
//! arithmetic (48-bit wrap) and CSE only hoists provably side-effect-free atoms.

use crate::ast::*;

pub fn optimize_program(prog: &mut Program) {
    // Run the pass pipeline to a fixpoint: folds expose new inlines, inlines
    // expose new folds/CSE. One pass leaves some on the table. Cap rounds so a
    // pathological input can't loop forever; in practice it converges in 2-3.
    // Convergence is detected by a structural fingerprint (Debug) of the program;
    // each pass is already semantics-preserving, so iterating can't change meaning.
    const MAX_ROUNDS: usize = 5;
    for _ in 0..MAX_ROUNDS {
        let before = format!("{prog:?}");
        opt_round(prog);
        if format!("{prog:?}") == before {
            break;
        }
    }
}

fn opt_round(prog: &mut Program) {
    inline_program(prog);
    for item in prog.iter_mut() {
        match item {
            Item::Fn(f) => {
                opt_block(&mut f.body);
                cse_block(&mut f.body);
                dce_block(&mut f.body);
                strip_srclines(&mut f.body);
            }
            Item::Struct(s) => {
                for m in s.methods.iter_mut() {
                    opt_block(&mut m.body);
                    cse_block(&mut m.body);
                    dce_block(&mut m.body);
                    strip_srclines(&mut m.body);
                }
            }
            Item::Stmt(s) => opt_stmt(s),
            Item::ExternBlock(_) | Item::Import(_) => {}
        }
    }

    let mut top: Vec<Stmt> = prog
        .iter()
        .filter_map(|it| {
            if let Item::Stmt(s) = it {
                Some(s.clone())
            } else {
                None
            }
        })
        .collect();
    if top.len() > 1 {
        dce_block(&mut top);

        let mut survivors = top.into_iter();
        for it in prog.iter_mut() {
            if let Item::Stmt(s) = it {
                if let Some(ns) = survivors.next() {
                    *s = ns;
                }
            }
        }
    }
}

struct Inlinable {
    params: Vec<String>,
    body: Expr,
}

fn inline_program(prog: &mut Program) {
    let mut cands: HashMap<String, Inlinable> = HashMap::new();
    for item in prog.iter() {
        if let Item::Fn(f) = item {
            if f.is_method {
                continue;
            }
            if let [Stmt::Return(Some(e))] = f.body.as_slice() {
                let params: Vec<String> = f.params.iter().map(|p| p.name.clone()).collect();

                // Only single-expression `return e` functions inline, and never a
                // self-recursive one (expr_calls guard) or inlining would loop. The
                // bounded fixpoint below re-runs a few times to catch chained inlines.
                if expr_calls(e, &f.name) {
                    continue;
                }
                cands.insert(
                    f.name.clone(),
                    Inlinable {
                        params,
                        body: e.clone(),
                    },
                );
            }
        }
    }
    if cands.is_empty() {
        return;
    }

    for _ in 0..5 {
        let mut changed = false;
        for item in prog.iter_mut() {
            match item {
                Item::Fn(f) => {
                    for s in f.body.iter_mut() {
                        inline_stmt(s, &cands, &mut changed);
                    }
                }
                Item::Struct(s) => {
                    for m in s.methods.iter_mut() {
                        for st in m.body.iter_mut() {
                            inline_stmt(st, &cands, &mut changed);
                        }
                    }
                }
                Item::Stmt(s) => inline_stmt(s, &cands, &mut changed),
                _ => {}
            }
        }
        if !changed {
            break;
        }
    }
}

fn expr_calls(e: &Expr, name: &str) -> bool {
    let mut found = false;
    walk_expr(e, &mut |x| {
        if let Expr::Call { callee, .. } = x {
            if matches!(&**callee, Expr::Ident(n) if n == name) {
                found = true;
            }
        }
    });
    found
}

fn pure_arg(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Nil | Expr::Ident(_)
    )
}

fn inline_stmt(s: &mut Stmt, cands: &HashMap<String, Inlinable>, changed: &mut bool) {
    match s {
        Stmt::Let { value, .. } | Stmt::ExprStmt(value) | Stmt::Return(Some(value)) => {
            inline_expr(value, cands, changed)
        }
        Stmt::Assign { target, value } => {
            inline_expr(target, cands, changed);
            inline_expr(value, cands, changed);
        }
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            inline_expr(cond, cands, changed);
            for st in then.iter_mut() {
                inline_stmt(st, cands, changed);
            }
            for (c, b) in elifs.iter_mut() {
                inline_expr(c, cands, changed);
                for st in b.iter_mut() {
                    inline_stmt(st, cands, changed);
                }
            }
            if let Some(b) = els {
                for st in b.iter_mut() {
                    inline_stmt(st, cands, changed);
                }
            }
        }
        Stmt::While { cond, body } => {
            inline_expr(cond, cands, changed);
            for st in body.iter_mut() {
                inline_stmt(st, cands, changed);
            }
        }
        Stmt::For { iter, body, .. } => {
            inline_expr(iter, cands, changed);
            for st in body.iter_mut() {
                inline_stmt(st, cands, changed);
            }
        }
        _ => {}
    }
}

fn inline_expr(e: &mut Expr, cands: &HashMap<String, Inlinable>, changed: &mut bool) {
    match e {
        Expr::Unary { expr, .. } => inline_expr(expr, cands, changed),
        Expr::Binary { lhs, rhs, .. } => {
            inline_expr(lhs, cands, changed);
            inline_expr(rhs, cands, changed);
        }
        Expr::Call { callee, args } => {
            inline_expr(callee, cands, changed);
            for a in args.iter_mut() {
                inline_expr(a, cands, changed);
            }
        }
        Expr::NamedCall { callee, args } => {
            inline_expr(callee, cands, changed);
            for (_, a) in args.iter_mut() {
                inline_expr(a, cands, changed);
            }
        }
        Expr::Method { obj, args, .. } => {
            inline_expr(obj, cands, changed);
            for a in args.iter_mut() {
                inline_expr(a, cands, changed);
            }
        }
        Expr::Field { obj, .. } => inline_expr(obj, cands, changed),
        Expr::Index { obj, index } => {
            inline_expr(obj, cands, changed);
            inline_expr(index, cands, changed);
        }
        Expr::IfElse { cond, then, els } => {
            inline_expr(cond, cands, changed);
            inline_expr(then, cands, changed);
            inline_expr(els, cands, changed);
        }
        Expr::List(xs) => {
            for x in xs.iter_mut() {
                inline_expr(x, cands, changed);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs.iter_mut() {
                inline_expr(k, cands, changed);
                inline_expr(v, cands, changed);
            }
        }
        Expr::Range { lo, hi } => {
            inline_expr(lo, cands, changed);
            inline_expr(hi, cands, changed);
        }
        Expr::Slice { obj, lo, hi } => {
            inline_expr(obj, cands, changed);
            if let Some(lo) = lo {
                inline_expr(lo, cands, changed);
            }
            if let Some(hi) = hi {
                inline_expr(hi, cands, changed);
            }
        }
        Expr::FStr(parts) => {
            for p in parts.iter_mut() {
                if let FStrPart::Expr(pe) = p {
                    inline_expr(pe, cands, changed);
                }
            }
        }
        _ => {}
    }

    if let Expr::Call { callee, args } = e {
        if let Expr::Ident(name) = &**callee {
            if let Some(c) = cands.get(name) {
                if c.params.len() == args.len() && args.iter().all(pure_arg) {
                    let map: HashMap<&str, &Expr> = c
                        .params
                        .iter()
                        .map(|p| p.as_str())
                        .zip(args.iter())
                        .collect();
                    let mut inlined = c.body.clone();
                    subst_params(&mut inlined, &map);
                    *e = inlined;
                    *changed = true;
                }
            }
        }
    }
}

fn subst_params(e: &mut Expr, map: &HashMap<&str, &Expr>) {
    match e {
        Expr::Ident(n) => {
            if let Some(replacement) = map.get(n.as_str()) {
                *e = (*replacement).clone();
            }
        }
        Expr::Unary { expr, .. } => subst_params(expr, map),
        Expr::Binary { lhs, rhs, .. } => {
            subst_params(lhs, map);
            subst_params(rhs, map);
        }
        Expr::Call { callee, args } => {
            subst_params(callee, map);
            for a in args.iter_mut() {
                subst_params(a, map);
            }
        }
        Expr::NamedCall { callee, args } => {
            subst_params(callee, map);
            for (_, a) in args.iter_mut() {
                subst_params(a, map);
            }
        }
        Expr::Method { obj, args, .. } => {
            subst_params(obj, map);
            for a in args.iter_mut() {
                subst_params(a, map);
            }
        }
        Expr::Field { obj, .. } => subst_params(obj, map),
        Expr::Index { obj, index } => {
            subst_params(obj, map);
            subst_params(index, map);
        }
        Expr::IfElse { cond, then, els } => {
            subst_params(cond, map);
            subst_params(then, map);
            subst_params(els, map);
        }
        Expr::List(xs) => {
            for x in xs.iter_mut() {
                subst_params(x, map);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs.iter_mut() {
                subst_params(k, map);
                subst_params(v, map);
            }
        }
        Expr::Range { lo, hi } => {
            subst_params(lo, map);
            subst_params(hi, map);
        }
        Expr::Slice { obj, lo, hi } => {
            subst_params(obj, map);
            if let Some(lo) = lo {
                subst_params(lo, map);
            }
            if let Some(hi) = hi {
                subst_params(hi, map);
            }
        }
        Expr::FStr(parts) => {
            for p in parts.iter_mut() {
                if let FStrPart::Expr(pe) = p {
                    subst_params(pe, map);
                }
            }
        }
        _ => {}
    }
}

fn walk_expr(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    match e {
        Expr::Unary { expr, .. } => walk_expr(expr, f),
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        Expr::Call { callee, args } => {
            walk_expr(callee, f);
            for a in args {
                walk_expr(a, f);
            }
        }
        Expr::NamedCall { callee, args } => {
            walk_expr(callee, f);
            for (_, a) in args {
                walk_expr(a, f);
            }
        }
        Expr::Method { obj, args, .. } => {
            walk_expr(obj, f);
            for a in args {
                walk_expr(a, f);
            }
        }
        Expr::Field { obj, .. } => walk_expr(obj, f),
        Expr::Index { obj, index } => {
            walk_expr(obj, f);
            walk_expr(index, f);
        }
        Expr::IfElse { cond, then, els } => {
            walk_expr(cond, f);
            walk_expr(then, f);
            walk_expr(els, f);
        }
        Expr::List(xs) => {
            for x in xs {
                walk_expr(x, f);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs {
                walk_expr(k, f);
                walk_expr(v, f);
            }
        }
        Expr::Range { lo, hi } => {
            walk_expr(lo, f);
            walk_expr(hi, f);
        }
        Expr::Slice { obj, lo, hi } => {
            walk_expr(obj, f);
            if let Some(lo) = lo {
                walk_expr(lo, f);
            }
            if let Some(hi) = hi {
                walk_expr(hi, f);
            }
        }
        Expr::FStr(parts) => {
            for p in parts {
                if let FStrPart::Expr(pe) = p {
                    walk_expr(pe, f);
                }
            }
        }
        _ => {}
    }
}

fn opt_block(body: &mut Vec<Stmt>) {

    let mut out: Vec<Stmt> = Vec::with_capacity(body.len());
    for mut s in body.drain(..) {
        opt_stmt(&mut s);
        match s {
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } => {
                match resolve_if(&cond, &elifs) {

                    IfPick::Block(which) => {
                        let chosen = pick_block(which, then, elifs, els);
                        out.extend(chosen);
                    }

                    IfPick::Unknown => out.push(Stmt::If {
                        cond,
                        then,
                        elifs,
                        els,
                    }),
                }
            }
            Stmt::While { cond, body } => {
                if matches!(cond, Expr::Bool(false)) {

                } else {
                    out.push(Stmt::While { cond, body });
                }
            }
            other => out.push(other),
        }
    }
    *body = out;
}

pub fn strip_srclines(body: &mut Vec<Stmt>) {
    for s in body.iter_mut() {
        match s {
            Stmt::If {
                then, elifs, els, ..
            } => {
                strip_srclines(then);
                for (_, b) in elifs.iter_mut() {
                    strip_srclines(b);
                }
                if let Some(b) = els {
                    strip_srclines(b);
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } => strip_srclines(body),
            Stmt::Try {
                body, catch_body, ..
            } => {
                strip_srclines(body);
                strip_srclines(catch_body);
            }
            _ => {}
        }
    }

    // SrcLine markers exist only to attribute runtime faults to a source line.
    // A marker is dead if the statement it precedes can never fault, so drop it.
    // This keeps line-number reporting accurate while shrinking the emitted code.
    let mut out: Vec<Stmt> = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if let Stmt::SrcLine(_) = &body[i] {
            if i + 1 < body.len() && !can_fault(&body[i + 1]) {

                i += 1;
                continue;
            }
        }
        out.push(body[i].clone());
        i += 1;
    }
    *body = out;
}

fn can_fault(s: &Stmt) -> bool {
    match s {
        Stmt::Let { value, .. } => may_fault(value),
        Stmt::Assign { target, value } => may_fault(target) || may_fault(value),
        Stmt::ExprStmt(e) => may_fault(e),
        Stmt::Return(opt) => opt.as_ref().map(may_fault).unwrap_or(false),

        Stmt::If { cond, elifs, .. } => {
            may_fault(cond) || elifs.iter().any(|(c, _)| may_fault(c))
        }
        Stmt::While { cond, .. } => may_fault(cond),
        Stmt::For { iter, .. } => may_fault(iter),

        Stmt::Try { .. } => false,
        Stmt::Raise(_) => true,
        Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => false,
    }
}

fn may_fault(e: &Expr) -> bool {
    match e {
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Nil | Expr::Str(_) => false,

        Expr::Ident(_) | Expr::SelfExpr => false,
        Expr::Unary { op, expr } => match op {

            UnOp::Neg | UnOp::Not => may_fault(expr),
        },
        Expr::Binary { op, lhs, rhs } => {
            if may_fault(lhs) || may_fault(rhs) {
                return true;
            }
            match op {

                BinOp::Div | BinOp::Mod => !matches!(&**rhs, Expr::Int(d) if *d != 0),

                BinOp::In | BinOp::NotIn => true,
                _ => false,
            }
        }
        Expr::Range { lo, hi } => may_fault(lo) || may_fault(hi),
        Expr::IfElse { cond, then, els } => {
            may_fault(cond) || may_fault(then) || may_fault(els)
        }

        _ => true,
    }
}

enum IfWhich {
    Then,
    Elif(usize),
    Else,
}
enum IfPick {
    Block(IfWhich),
    Unknown,
}

fn resolve_if(cond: &Expr, elifs: &[(Expr, Vec<Stmt>)]) -> IfPick {
    match cond {
        Expr::Bool(true) => return IfPick::Block(IfWhich::Then),
        Expr::Bool(false) => {}
        _ => return IfPick::Unknown,
    }
    for (i, (c, _)) in elifs.iter().enumerate() {
        match c {
            Expr::Bool(true) => return IfPick::Block(IfWhich::Elif(i)),
            Expr::Bool(false) => continue,
            _ => return IfPick::Unknown,
        }
    }
    IfPick::Block(IfWhich::Else)
}

fn pick_block(
    which: IfWhich,
    then: Vec<Stmt>,
    elifs: Vec<(Expr, Vec<Stmt>)>,
    els: Option<Vec<Stmt>>,
) -> Vec<Stmt> {
    match which {
        IfWhich::Then => then,
        IfWhich::Elif(i) => elifs.into_iter().nth(i).unwrap().1,
        IfWhich::Else => els.unwrap_or_default(),
    }
}

fn opt_stmt(s: &mut Stmt) {
    match s {
        Stmt::Let { value, .. } => fold_here(value),
        Stmt::Assign { target, value } => {
            fold_here(target);
            fold_here(value);
        }
        Stmt::ExprStmt(e) => fold_here(e),
        Stmt::Return(Some(e)) => fold_here(e),
        Stmt::Return(None) => {}
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            fold_here(cond);
            opt_block(then);
            for (c, b) in elifs.iter_mut() {
                fold_here(c);
                opt_block(b);
            }
            if let Some(b) = els {
                opt_block(b);
            }

        }
        Stmt::While { cond, body } => {
            fold_here(cond);
            opt_block(body);
        }
        Stmt::For { iter, body, .. } => {
            fold_here(iter);
            opt_block(body);
        }
        Stmt::Try {
            body, catch_body, ..
        } => {
            opt_block(body);
            opt_block(catch_body);
        }
        Stmt::Raise(e) => fold_here(e),
        Stmt::Break | Stmt::Continue => {}
        Stmt::SrcLine(_) => {}
    }
}

fn fold_here(e: &mut Expr) {
    match e {
        Expr::Unary { expr, .. } => fold_here(expr),
        Expr::Binary { lhs, rhs, .. } => {
            fold_here(lhs);
            fold_here(rhs);
        }
        Expr::Call { callee, args } => {
            fold_here(callee);
            for a in args.iter_mut() {
                fold_here(a);
            }
        }
        Expr::NamedCall { callee, args } => {
            fold_here(callee);
            for (_, a) in args.iter_mut() {
                fold_here(a);
            }
        }
        Expr::Method { obj, args, .. } => {
            fold_here(obj);
            for a in args.iter_mut() {
                fold_here(a);
            }
        }
        Expr::Field { obj, .. } => fold_here(obj),
        Expr::Index { obj, index } => {
            fold_here(obj);
            fold_here(index);
        }
        Expr::List(xs) => {
            for x in xs.iter_mut() {
                fold_here(x);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs.iter_mut() {
                fold_here(k);
                fold_here(v);
            }
        }
        Expr::Range { lo, hi } => {
            fold_here(lo);
            fold_here(hi);
        }
        Expr::FStr(parts) => {
            for p in parts.iter_mut() {
                if let FStrPart::Expr(pe) = p {
                    fold_here(pe);
                }
            }
        }
        _ => {}
    }

    if let Some(folded) = try_fold(e) {
        *e = folded;
    }
}

fn try_fold(e: &Expr) -> Option<Expr> {
    match e {
        Expr::Unary { op, expr } => match (op, &**expr) {
            (UnOp::Neg, Expr::Int(n)) => n.checked_neg().map(Expr::Int),
            (UnOp::Neg, Expr::Float(x)) => Some(Expr::Float(-x)),
            (UnOp::Not, Expr::Bool(b)) => Some(Expr::Bool(!b)),
            _ => None,
        },
        Expr::Binary { op, lhs, rhs } => fold_binary(*op, lhs, rhs),
        _ => None,
    }
}

fn fold_binary(op: BinOp, lhs: &Expr, rhs: &Expr) -> Option<Expr> {
    use BinOp::*;
    use Expr::*;

    let as_floats = |a: &Expr, b: &Expr| -> Option<(f64, f64)> {
        match (a, b) {
            (Float(x), Float(y)) => Some((*x, *y)),
            (Int(x), Float(y)) => Some((*x as f64, *y)),
            (Float(x), Int(y)) => Some((*x, *y as f64)),
            _ => None,
        }
    };

    match (op, lhs, rhs) {

        // Fold integer arithmetic with the SAME wrap48-after-wrapping-op that interp
        // uses at runtime, so a folded constant equals what either backend would have
        // computed. Div/Mod by zero is left unfolded (return None) so the fault still
        // happens at runtime rather than being silently dropped here.
        (Add, Int(a), Int(b)) => Some(Int(wrap48(a.wrapping_add(*b)))),
        (Sub, Int(a), Int(b)) => Some(Int(wrap48(a.wrapping_sub(*b)))),
        (Mul, Int(a), Int(b)) => Some(Int(wrap48(a.wrapping_mul(*b)))),
        (Div, Int(a), Int(b)) => {
            if *b == 0 {
                None
            } else {
                Some(Int(wrap48(a.wrapping_div(*b))))
            }
        }
        (Mod, Int(a), Int(b)) => {
            if *b == 0 {
                None
            } else {
                Some(Int(wrap48(a.wrapping_rem(*b))))
            }
        }

        (Pow, Int(a), Int(b)) => {
            if *b >= 0 {
                let mut acc: i64 = 1;
                let mut i: i64 = 0;
                while i < *b {
                    acc = wrap48(acc.wrapping_mul(*a));
                    i += 1;
                }
                Some(Int(acc))
            } else {
                Some(Float((*a as f64).powf(*b as f64)))
            }
        }
        (Pow, _, _) if as_floats(lhs, rhs).is_some() => {
            let (a, b) = as_floats(lhs, rhs).unwrap();
            Some(Float(a.powf(b)))
        }

        (Add, _, _) if as_floats(lhs, rhs).is_some() => {
            let (a, b) = as_floats(lhs, rhs).unwrap();
            Some(Float(a + b))
        }
        (Sub, _, _) if as_floats(lhs, rhs).is_some() => {
            let (a, b) = as_floats(lhs, rhs).unwrap();
            Some(Float(a - b))
        }
        (Mul, _, _) if as_floats(lhs, rhs).is_some() => {
            let (a, b) = as_floats(lhs, rhs).unwrap();
            Some(Float(a * b))
        }
        (Div, _, _) if as_floats(lhs, rhs).is_some() => {
            let (a, b) = as_floats(lhs, rhs).unwrap();
            Some(Float(a / b))
        }
        (Mod, _, _) if as_floats(lhs, rhs).is_some() => {
            let (a, b) = as_floats(lhs, rhs).unwrap();
            Some(Float(a % b))
        }

        (Add, Str(a), Str(b)) => Some(Str(std::rc::Rc::new(format!("{}{}", a, b)))),

        (Eq, _, _) => lit_eq(lhs, rhs).map(Bool),
        (Ne, _, _) => lit_eq(lhs, rhs).map(|b| Bool(!b)),

        (Lt, Int(a), Int(b)) => Some(Bool(a < b)),
        (Le, Int(a), Int(b)) => Some(Bool(a <= b)),
        (Gt, Int(a), Int(b)) => Some(Bool(a > b)),
        (Ge, Int(a), Int(b)) => Some(Bool(a >= b)),

        (Lt, _, _) if as_floats(lhs, rhs).is_some() => {
            let (a, b) = as_floats(lhs, rhs).unwrap();
            Some(Bool(a < b))
        }
        (Le, _, _) if as_floats(lhs, rhs).is_some() => {
            let (a, b) = as_floats(lhs, rhs).unwrap();
            Some(Bool(a <= b))
        }
        (Gt, _, _) if as_floats(lhs, rhs).is_some() => {
            let (a, b) = as_floats(lhs, rhs).unwrap();
            Some(Bool(a > b))
        }
        (Ge, _, _) if as_floats(lhs, rhs).is_some() => {
            let (a, b) = as_floats(lhs, rhs).unwrap();
            Some(Bool(a >= b))
        }

        (Lt, Str(a), Str(b)) => Some(Bool(a < b)),
        (Le, Str(a), Str(b)) => Some(Bool(a <= b)),
        (Gt, Str(a), Str(b)) => Some(Bool(a > b)),
        (Ge, Str(a), Str(b)) => Some(Bool(a >= b)),

        (And, Bool(a), Bool(b)) => Some(Bool(*a && *b)),
        (Or, Bool(a), Bool(b)) => Some(Bool(*a || *b)),

        _ => None,
    }
}

fn lit_eq(a: &Expr, b: &Expr) -> Option<bool> {
    use Expr::*;
    match (a, b) {
        (Int(x), Int(y)) => Some(x == y),
        (Float(x), Float(y)) => Some(x == y),
        (Str(x), Str(y)) => Some(x == y),
        (Bool(x), Bool(y)) => Some(x == y),
        (Nil, Nil) => Some(true),

        (Int(_), Float(_)) | (Float(_), Int(_)) => Some(false),
        (
            Int(_) | Float(_) | Str(_) | Bool(_) | Nil,
            Int(_) | Float(_) | Str(_) | Bool(_) | Nil,
        ) => Some(false),
        _ => None,
    }
}

fn dce_block(body: &mut Vec<Stmt>) {
    for s in body.iter_mut() {
        match s {
            Stmt::If {
                then, elifs, els, ..
            } => {
                dce_block(then);
                for (_, b) in elifs.iter_mut() {
                    dce_block(b);
                }
                if let Some(b) = els {
                    dce_block(b);
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } => dce_block(body),
            Stmt::Try {
                body, catch_body, ..
            } => {
                dce_block(body);
                dce_block(catch_body);
            }
            _ => {}
        }
    }

    let mut i = 0;
    while i < body.len() {
        // Only drop a `let` when its initializer is side-effect-free AND the name is
        // never read or reassigned in the rest of the block. Both conditions matter:
        // a fallible/effecting RHS must stay even if unused, so removability is gated
        // on is_removable, not just the read count.
        let remove = if let Stmt::Let { name, value, .. } = &body[i] {
            is_removable(value)
                && count_reads(name, body) == 0
                && count_assigns(name, body) == 0
        } else {
            false
        };
        if remove {
            body.remove(i);
        } else {
            i += 1;
        }
    }
}

fn is_removable(e: &Expr) -> bool {
    match e {
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Nil => true,

        Expr::Ident(_) | Expr::SelfExpr => true,
        Expr::List(xs) => xs.iter().all(is_removable),
        Expr::Map(kvs) => kvs.iter().all(|(k, v)| is_removable(k) && is_removable(v)),
        _ => false,
    }
}

fn count_reads(name: &str, body: &[Stmt]) -> usize {
    body.iter().map(|s| reads_stmt(name, s)).sum()
}

fn reads_stmt(name: &str, s: &Stmt) -> usize {
    match s {
        Stmt::Let { value, .. } => reads_expr(name, value),
        Stmt::Assign { target, value } => {

            let t = match target {
                Expr::Ident(_) => 0,
                other => reads_expr(name, other),
            };
            t + reads_expr(name, value)
        }
        Stmt::ExprStmt(e) => reads_expr(name, e),
        Stmt::Return(Some(e)) => reads_expr(name, e),
        Stmt::Return(None) => 0,
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            let mut n = reads_expr(name, cond) + count_reads(name, then);
            for (c, b) in elifs {
                n += reads_expr(name, c) + count_reads(name, b);
            }
            if let Some(b) = els {
                n += count_reads(name, b);
            }
            n
        }
        Stmt::While { cond, body } => reads_expr(name, cond) + count_reads(name, body),
        Stmt::For { var, iter, body } => {

            let _ = var;
            reads_expr(name, iter) + count_reads(name, body)
        }
        Stmt::Try {
            body, catch_body, ..
        } => count_reads(name, body) + count_reads(name, catch_body),
        Stmt::Raise(e) => reads_expr(name, e),
        Stmt::Break | Stmt::Continue => 0,
        Stmt::SrcLine(_) => 0,
    }
}

fn reads_expr(name: &str, e: &Expr) -> usize {
    match e {
        Expr::Ident(n) => (n == name) as usize,
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Nil
        | Expr::SelfExpr => 0,
        Expr::Unary { expr, .. } => reads_expr(name, expr),
        Expr::Binary { lhs, rhs, .. } => reads_expr(name, lhs) + reads_expr(name, rhs),
        Expr::Call { callee, args } => {
            reads_expr(name, callee) + args.iter().map(|a| reads_expr(name, a)).sum::<usize>()
        }
        Expr::NamedCall { callee, args } => {
            reads_expr(name, callee)
                + args
                    .iter()
                    .map(|(_, a)| reads_expr(name, a))
                    .sum::<usize>()
        }
        Expr::Method { obj, args, .. } => {
            reads_expr(name, obj) + args.iter().map(|a| reads_expr(name, a)).sum::<usize>()
        }
        Expr::Field { obj, .. } => reads_expr(name, obj),
        Expr::Index { obj, index } => reads_expr(name, obj) + reads_expr(name, index),
        Expr::List(xs) => xs.iter().map(|x| reads_expr(name, x)).sum(),
        Expr::Map(kvs) => kvs
            .iter()
            .map(|(k, v)| reads_expr(name, k) + reads_expr(name, v))
            .sum(),
        Expr::Range { lo, hi } => reads_expr(name, lo) + reads_expr(name, hi),
        Expr::IfElse { cond, then, els } => {
            reads_expr(name, cond) + reads_expr(name, then) + reads_expr(name, els)
        }
        Expr::ListComp {
            elem, iter, cond, ..
        } => {

            reads_expr(name, elem)
                + reads_expr(name, iter)
                + cond.as_ref().map_or(0, |c| reads_expr(name, c))
        }
        Expr::Slice { obj, lo, hi } => {
            reads_expr(name, obj)
                + lo.as_ref().map_or(0, |e| reads_expr(name, e))
                + hi.as_ref().map_or(0, |e| reads_expr(name, e))
        }

        Expr::Lambda { .. } => 0,

        Expr::Closure { captures, .. } => captures.iter().map(|c| reads_expr(name, c)).sum(),
        Expr::FStr(parts) => parts
            .iter()
            .map(|p| match p {
                FStrPart::Expr(pe) => reads_expr(name, pe),
                FStrPart::Lit(_) => 0,
            })
            .sum(),
    }
}

fn count_assigns(name: &str, body: &[Stmt]) -> usize {
    body.iter().map(|s| stmt_assigns(name, s)).sum()
}

fn stmt_assigns(name: &str, s: &Stmt) -> usize {
    match s {
        Stmt::Assign {
            target: Expr::Ident(n),
            ..
        } => (n == name) as usize,
        Stmt::If {
            then, elifs, els, ..
        } => {
            let mut n = count_assigns(name, then);
            for (_, b) in elifs {
                n += count_assigns(name, b);
            }
            if let Some(b) = els {
                n += count_assigns(name, b);
            }
            n
        }
        Stmt::While { body, .. } | Stmt::For { body, .. } => count_assigns(name, body),
        Stmt::Try {
            body, catch_body, ..
        } => count_assigns(name, body) + count_assigns(name, catch_body),
        _ => 0,
    }
}

use std::collections::HashMap;
use std::collections::HashSet;

fn cse_block(body: &mut Vec<Stmt>) {
    for s in body.iter_mut() {
        match s {
            Stmt::If {
                then, elifs, els, ..
            } => {
                cse_block(then);
                for (_, b) in elifs.iter_mut() {
                    cse_block(b);
                }
                if let Some(b) = els {
                    cse_block(b);
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } => cse_block(body),
            Stmt::Try {
                body, catch_body, ..
            } => {
                cse_block(body);
                cse_block(catch_body);
            }
            _ => {}
        }
    }

    let mut assigned: HashSet<String> = HashSet::new();
    for s in body.iter() {
        collect_assigns(s, &mut assigned);
    }

    let mut counter: u32 = 0;
    let mut out: Vec<Stmt> = Vec::with_capacity(body.len());
    for mut s in std::mem::take(body) {
        let hoists = cse_stmt(&mut s, &assigned, &mut counter);
        out.extend(hoists);
        out.push(s);
    }
    *body = out;
}

fn cse_stmt(s: &mut Stmt, assigned: &HashSet<String>, counter: &mut u32) -> Vec<Stmt> {
    let mut counts: HashMap<String, (Expr, usize)> = HashMap::new();
    each_expr(s, &mut |e| collect_cse(e, assigned, &mut counts));

    let mut hoistable: Vec<(String, Expr)> = counts
        .into_iter()
        .filter(|(_, (_, n))| *n >= 2)
        .map(|(k, (e, _))| (k, e))
        .collect();
    hoistable.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));

    let mut hoists = Vec::new();
    for (key, expr) in hoistable {
        let mut occ = 0usize;
        each_expr(s, &mut |e| occ += count_occurrences(e, &key));
        if occ < 2 {
            continue;
        }
        let name = format!("__cse_{}", *counter);
        *counter += 1;
        each_expr(s, &mut |e| replace_occurrences(e, &key, &name));
        hoists.push(Stmt::Let {
            name,
            mutable: false,
            ty: Type::Unknown,
            value: expr,
        });
    }
    hoists
}

fn cse_key(e: &Expr, assigned: &HashSet<String>) -> Option<String> {
    // CSE is only safe for pure, deterministic atoms, so we build keys ONLY from
    // int/float literals, never-reassigned idents (`!assigned`), and +/-/* over
    // those. Anything that could fault, call, or read a mutable binding returns
    // None and is never hoisted. Reassigned variables are excluded because their
    // value can differ between the two occurrences.
    fn atom_key(e: &Expr, assigned: &HashSet<String>) -> Option<String> {
        match e {
            Expr::Int(n) => Some(format!("i{}", n)),
            Expr::Float(x) => Some(format!("f{}", x.to_bits())),
            Expr::Ident(n) if !assigned.contains(n) => Some(format!("v{}", n)),
            Expr::Unary {
                op: UnOp::Neg,
                expr,
            } => atom_key(expr, assigned).map(|k| format!("(neg {})", k)),
            Expr::Binary { op, lhs, rhs } if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) => {
                let l = atom_key(lhs, assigned)?;
                let r = atom_key(rhs, assigned)?;
                Some(format!("({:?} {} {})", op, l, r))
            }
            _ => None,
        }
    }
    match e {
        Expr::Binary {
            op: BinOp::Add | BinOp::Sub | BinOp::Mul,
            ..
        } => atom_key(e, assigned),
        Expr::Unary { op: UnOp::Neg, .. } => atom_key(e, assigned),
        _ => None,
    }
}

fn collect_cse(
    e: &Expr,
    assigned: &HashSet<String>,
    counts: &mut HashMap<String, (Expr, usize)>,
) {
    if let Some(k) = cse_key(e, assigned) {
        let entry = counts.entry(k).or_insert_with(|| (e.clone(), 0));
        entry.1 += 1;
    }
    walk_kids(e, &mut |c| collect_cse(c, assigned, counts));
}

fn count_occurrences(e: &Expr, key: &str) -> usize {
    if expr_match(e, key) {
        return 1;
    }
    let mut n = 0;
    walk_kids(e, &mut |c| n += count_occurrences(c, key));
    n
}

fn replace_occurrences(e: &mut Expr, key: &str, name: &str) {
    if expr_match(e, key) {
        *e = Expr::Ident(name.to_string());
        return;
    }
    walk_kidsm(e, &mut |c| replace_occurrences(c, key, name));
}

fn expr_match(e: &Expr, key: &str) -> bool {
    structural_key(e).as_deref() == Some(key)
}

fn structural_key(e: &Expr) -> Option<String> {
    match e {
        Expr::Int(n) => Some(format!("i{}", n)),
        Expr::Float(x) => Some(format!("f{}", x.to_bits())),
        Expr::Ident(n) => Some(format!("v{}", n)),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
        } => structural_key(expr).map(|k| format!("(neg {})", k)),
        Expr::Binary { op, lhs, rhs } if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) => {
            let l = structural_key(lhs)?;
            let r = structural_key(rhs)?;
            Some(format!("({:?} {} {})", op, l, r))
        }
        _ => None,
    }
}

fn walk_kids(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    match e {
        Expr::Unary { expr, .. } => f(expr),
        Expr::Binary { op, lhs, rhs } => {
            f(lhs);
            if !matches!(op, BinOp::And | BinOp::Or) {
                f(rhs);
            }
        }
        Expr::Call { callee, args } => {
            f(callee);
            for a in args {
                f(a);
            }
        }
        Expr::NamedCall { callee, args } => {
            f(callee);
            for (_, a) in args {
                f(a);
            }
        }
        Expr::Method { obj, args, .. } => {
            f(obj);
            for a in args {
                f(a);
            }
        }
        Expr::Field { obj, .. } => f(obj),
        Expr::Index { obj, index } => {
            f(obj);
            f(index);
        }
        Expr::List(xs) => {
            for x in xs {
                f(x);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs {
                f(k);
                f(v);
            }
        }
        Expr::Range { lo, hi } => {
            f(lo);
            f(hi);
        }
        Expr::FStr(parts) => {
            for p in parts {
                if let FStrPart::Expr(pe) = p {
                    f(pe);
                }
            }
        }
        Expr::Closure { captures, .. } => {
            for c in captures {
                f(c);
            }
        }
        _ => {}
    }
}

fn walk_kidsm(e: &mut Expr, f: &mut dyn FnMut(&mut Expr)) {
    match e {
        Expr::Unary { expr, .. } => f(expr),
        Expr::Binary { op, lhs, rhs } => {
            let short = matches!(op, BinOp::And | BinOp::Or);
            f(lhs);
            if !short {
                f(rhs);
            }
        }
        Expr::Call { callee, args } => {
            f(callee);
            for a in args {
                f(a);
            }
        }
        Expr::NamedCall { callee, args } => {
            f(callee);
            for (_, a) in args {
                f(a);
            }
        }
        Expr::Method { obj, args, .. } => {
            f(obj);
            for a in args {
                f(a);
            }
        }
        Expr::Field { obj, .. } => f(obj),
        Expr::Index { obj, index } => {
            f(obj);
            f(index);
        }
        Expr::List(xs) => {
            for x in xs {
                f(x);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs {
                f(k);
                f(v);
            }
        }
        Expr::Range { lo, hi } => {
            f(lo);
            f(hi);
        }
        Expr::FStr(parts) => {
            for p in parts {
                if let FStrPart::Expr(pe) = p {
                    f(pe);
                }
            }
        }
        Expr::Closure { captures, .. } => {
            for c in captures {
                f(c);
            }
        }
        _ => {}
    }
}

fn each_expr(s: &mut Stmt, f: &mut dyn FnMut(&mut Expr)) {
    match s {
        Stmt::Let { value, .. } => f(value),
        Stmt::Assign { target, value } => {
            f(target);
            f(value);
        }
        Stmt::ExprStmt(e) => f(e),
        Stmt::Return(Some(e)) => f(e),
        Stmt::If { cond, .. } => f(cond),
        Stmt::While { cond, .. } => f(cond),
        Stmt::For { iter, .. } => f(iter),
        _ => {}
    }
}

fn collect_assigns(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::Assign {
            target: Expr::Ident(n),
            ..
        } => {
            out.insert(n.clone());
        }
        Stmt::If {
            then, elifs, els, ..
        } => {
            for s in then {
                collect_assigns(s, out);
            }
            for (_, b) in elifs {
                for s in b {
                    collect_assigns(s, out);
                }
            }
            if let Some(b) = els {
                for s in b {
                    collect_assigns(s, out);
                }
            }
        }
        Stmt::While { body, .. } | Stmt::For { body, .. } => {
            for s in body {
                collect_assigns(s, out);
            }
        }
        Stmt::Try {
            body, catch_body, ..
        } => {
            for s in body {
                collect_assigns(s, out);
            }
            for s in catch_body {
                collect_assigns(s, out);
            }
        }
        _ => {}
    }
}
