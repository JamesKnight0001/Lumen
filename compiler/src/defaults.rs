//! Default-argument expansion. Collects each free function's trailing param
//! defaults, then pads call sites that omit those args. Runs after imports are
//! merged and before desugar, so both backends see fully-applied calls - no
//! codegen/interp changes, no runtime cost, output stays byte-identical.
use crate::ast::*;
use std::collections::HashMap;

// fn name -> its param defaults (None = required, Some = has a default expr).
type Defaults = HashMap<String, Vec<Option<Expr>>>;

pub fn expand_program(prog: &mut Program) -> Result<(), crate::CompileError> {
    let mut defs: Defaults = HashMap::new();
    for item in prog.iter() {
        if let Item::Fn(f) = item {
            let ds: Vec<Option<Expr>> = f.params.iter().map(|p| p.default.clone()).collect();
            validate(&f.name, &ds)?;
            if ds.iter().any(|d| d.is_some()) {
                defs.insert(f.name.clone(), ds);
            }
        }
    }
    for item in prog.iter_mut() {
        match item {
            Item::Fn(f) => walk_block(&mut f.body, &defs),
            Item::Struct(s) => {
                for m in s.methods.iter_mut() {
                    walk_block(&mut m.body, &defs);
                }
            }
            Item::Stmt(s) => walk_stmt(s, &defs),
            Item::ExternBlock(_) | Item::Import(_) => {}
        }
    }
    Ok(())
}

// A defaulted param can't be followed by a required one (call padding is
// trailing-only, so a gap would be unfillable).
fn validate(name: &str, ds: &[Option<Expr>]) -> Result<(), crate::CompileError> {
    let mut seen = false;
    for d in ds {
        if d.is_some() {
            seen = true;
        } else if seen {
            return Err(crate::CompileError::Parse(format!(
                "fn {name}: a parameter without a default cannot follow one with a default"
            )));
        }
    }
    Ok(())
}

// Pad a call to `name` if it omits trailing defaulted args. Caller-supplied
// args always win; only the missing tail is filled from the defaults.
fn pad(name: &str, args: &mut Vec<Expr>, defs: &Defaults) {
    let Some(ds) = defs.get(name) else { return };
    if args.len() >= ds.len() {
        return;
    }
    for d in &ds[args.len()..] {
        match d {
            Some(e) => args.push(e.clone()),
            None => return, // hit a required slot: leave it, backend will error
        }
    }
}

fn walk_block(body: &mut [Stmt], defs: &Defaults) {
    for s in body.iter_mut() {
        walk_stmt(s, defs);
    }
}

fn walk_stmt(s: &mut Stmt, defs: &Defaults) {
    match s {
        Stmt::Let { value, .. } => walk_expr(value, defs),
        Stmt::Assign { target, value } => {
            walk_expr(target, defs);
            walk_expr(value, defs);
        }
        Stmt::ExprStmt(e) | Stmt::Return(Some(e)) | Stmt::Raise(e) => walk_expr(e, defs),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => {}
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            walk_expr(cond, defs);
            walk_block(then, defs);
            for (c, b) in elifs.iter_mut() {
                walk_expr(c, defs);
                walk_block(b, defs);
            }
            if let Some(b) = els {
                walk_block(b, defs);
            }
        }
        Stmt::While { cond, body } => {
            walk_expr(cond, defs);
            walk_block(body, defs);
        }
        Stmt::For { iter, body, .. } => {
            walk_expr(iter, defs);
            walk_block(body, defs);
        }
        Stmt::Try {
            body, catch_body, ..
        } => {
            walk_block(body, defs);
            walk_block(catch_body, defs);
        }
    }
}

fn walk_expr(e: &mut Expr, defs: &Defaults) {
    match e {
        Expr::Call { callee, args } => {
            for a in args.iter_mut() {
                walk_expr(a, defs);
            }
            if let Expr::Ident(name) = callee.as_ref() {
                pad(name, args, defs);
            }
            walk_expr(callee, defs);
        }
        Expr::Unary { expr, .. } => walk_expr(expr, defs),
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, defs);
            walk_expr(rhs, defs);
        }
        Expr::NamedCall { callee, args } => {
            walk_expr(callee, defs);
            for (_, a) in args.iter_mut() {
                walk_expr(a, defs);
            }
        }
        Expr::Method { obj, args, .. } => {
            walk_expr(obj, defs);
            for a in args.iter_mut() {
                walk_expr(a, defs);
            }
        }
        Expr::Field { obj, .. } => walk_expr(obj, defs),
        Expr::Index { obj, index } => {
            walk_expr(obj, defs);
            walk_expr(index, defs);
        }
        Expr::Slice { obj, lo, hi } => {
            walk_expr(obj, defs);
            if let Some(lo) = lo {
                walk_expr(lo, defs);
            }
            if let Some(hi) = hi {
                walk_expr(hi, defs);
            }
        }
        Expr::List(xs) => {
            for x in xs.iter_mut() {
                walk_expr(x, defs);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs.iter_mut() {
                walk_expr(k, defs);
                walk_expr(v, defs);
            }
        }
        Expr::Range { lo, hi } => {
            walk_expr(lo, defs);
            walk_expr(hi, defs);
        }
        Expr::IfElse { cond, then, els } => {
            walk_expr(cond, defs);
            walk_expr(then, defs);
            walk_expr(els, defs);
        }
        Expr::ListComp {
            elem, iter, cond, ..
        } => {
            walk_expr(elem, defs);
            walk_expr(iter, defs);
            if let Some(c) = cond {
                walk_expr(c, defs);
            }
        }
        Expr::Lambda { body, .. } => walk_block(body, defs),
        Expr::FStr(parts) => {
            for p in parts.iter_mut() {
                if let FStrPart::Expr(pe) = p {
                    walk_expr(pe, defs);
                }
            }
        }
        _ => {}
    }
}
