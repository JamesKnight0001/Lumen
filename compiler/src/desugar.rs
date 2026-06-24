//! Desugaring pass: rewrites surface conveniences into the small core the
//! backends understand. Right now it lowers `.map(f)` and `.filter(f)` method
//! calls into list comprehensions, which a later pass already knows how to emit.
use crate::ast::*;

pub fn desugar_program(prog: &mut Program) {
    let mut n: usize = 0;
    for item in prog.iter_mut() {
        desugar_item(item, &mut n);
    }
}

fn desugar_item(item: &mut Item, n: &mut usize) {
    match item {
        Item::Fn(f) => desugar_block(&mut f.body, n),
        Item::Struct(s) => {
            for m in s.methods.iter_mut() {
                desugar_block(&mut m.body, n);
            }
        }
        Item::Stmt(s) => desugar_stmt(s, n),
        Item::ExternBlock(_) | Item::Import(_) => {}
    }
}

fn desugar_block(body: &mut [Stmt], n: &mut usize) {
    for s in body.iter_mut() {
        desugar_stmt(s, n);
    }
}

fn desugar_stmt(s: &mut Stmt, n: &mut usize) {
    match s {
        Stmt::Let { value, .. } => desugar_expr(value, n),
        Stmt::Assign { target, value } => {
            desugar_expr(target, n);
            desugar_expr(value, n);
        }
        Stmt::ExprStmt(e) => desugar_expr(e, n),
        Stmt::Return(Some(e)) => desugar_expr(e, n),
        Stmt::Return(None) => {}
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            desugar_expr(cond, n);
            desugar_block(then, n);
            for (c, b) in elifs.iter_mut() {
                desugar_expr(c, n);
                desugar_block(b, n);
            }
            if let Some(b) = els {
                desugar_block(b, n);
            }
        }
        Stmt::While { cond, body } => {
            desugar_expr(cond, n);
            desugar_block(body, n);
        }
        Stmt::For { iter, body, .. } => {
            desugar_expr(iter, n);
            desugar_block(body, n);
        }
        Stmt::Try {
            body, catch_body, ..
        } => {
            desugar_block(body, n);
            desugar_block(catch_body, n);
        }
        Stmt::Raise(e) => desugar_expr(e, n),
        Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => {}
    }
}

fn desugar_expr(e: &mut Expr, n: &mut usize) {
    match e {
        Expr::Unary { expr, .. } => desugar_expr(expr, n),
        Expr::Binary { lhs, rhs, .. } => {
            desugar_expr(lhs, n);
            desugar_expr(rhs, n);
        }
        Expr::Call { callee, args } => {
            desugar_expr(callee, n);
            for a in args.iter_mut() {
                desugar_expr(a, n);
            }
        }
        Expr::NamedCall { callee, args } => {
            desugar_expr(callee, n);
            for (_, a) in args.iter_mut() {
                desugar_expr(a, n);
            }
        }
        Expr::Method { obj, args, .. } => {
            desugar_expr(obj, n);
            for a in args.iter_mut() {
                desugar_expr(a, n);
            }
        }
        Expr::Field { obj, .. } => desugar_expr(obj, n),
        Expr::Index { obj, index } => {
            desugar_expr(obj, n);
            desugar_expr(index, n);
        }
        Expr::Slice { obj, lo, hi } => {
            desugar_expr(obj, n);
            if let Some(lo) = lo {
                desugar_expr(lo, n);
            }
            if let Some(hi) = hi {
                desugar_expr(hi, n);
            }
        }
        Expr::List(xs) => {
            for x in xs.iter_mut() {
                desugar_expr(x, n);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs.iter_mut() {
                desugar_expr(k, n);
                desugar_expr(v, n);
            }
        }
        Expr::Range { lo, hi } => {
            desugar_expr(lo, n);
            desugar_expr(hi, n);
        }
        Expr::IfElse { cond, then, els } => {
            desugar_expr(cond, n);
            desugar_expr(then, n);
            desugar_expr(els, n);
        }
        Expr::ListComp {
            elem, iter, cond, ..
        } => {
            desugar_expr(elem, n);
            desugar_expr(iter, n);
            if let Some(c) = cond {
                desugar_expr(c, n);
            }
        }
        Expr::FStr(parts) => {
            for p in parts.iter_mut() {
                if let FStrPart::Expr(pe) = p {
                    desugar_expr(pe, n);
                }
            }
        }
        _ => {}
    }

    // Lower xs.map(f) into [f(#mf) for #mf in xs] and xs.filter(f) into
    // [#mf for #mf in xs if f(#mf)]. The `#mf{n}` name is unspellable in source,
    // so it can't collide with a user variable. Children are desugared first
    // (above) so nested map/filter compose correctly.
    if let Expr::Method { obj, name, args } = e {
        if args.len() == 1 && (name == "map" || name == "filter") {
            let is_map = name == "map";
            *n += 1;
            let var = format!("#mf{}", *n);
            let f = args.pop().unwrap();
            let iter = std::mem::replace(obj.as_mut(), Expr::Nil);
            let call_f = Expr::Call {
                callee: Box::new(f),
                args: vec![Expr::Ident(var.clone())],
            };
            *e = if is_map {
                Expr::ListComp {
                    elem: Box::new(call_f),
                    var,
                    iter: Box::new(iter),
                    cond: None,
                }
            } else {
                Expr::ListComp {
                    elem: Box::new(Expr::Ident(var.clone())),
                    var,
                    iter: Box::new(iter),
                    cond: Some(Box::new(call_f)),
                }
            };
        }
    }
}
