
//! Resolves `import` statements: reads each referenced .lm file, parses it, and
//! splices its top-level fns/structs into the program. Qualified references like
//! `mymod.foo(x)` are rewritten to bare `foo(x)`. Note the limitation: all
//! imported items land in one shared global namespace, so names from different
//! modules can collide. Built-in modules (math, os, ...) are left alone here.
use crate::ast::{Expr, FStrPart, Item, Program, Stmt};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

fn module_to_path(base_dir: &Path, module: &str) -> (PathBuf, String) {
    let segs: Vec<&str> = module.split('.').collect();
    let access = segs.last().copied().unwrap_or(module).to_string();

    // First the compiler's original rule: sibling file relative to the entry
    // dir. If that exists, use it (keeps every existing project working).
    let mut sib = base_dir.to_path_buf();
    for s in &segs {
        sib.push(s);
    }
    sib.set_extension("lm");
    if sib.exists() {
        return (sib, access);
    }

    // Else search package dirs (installed via `lumen install`): the active venv
    // (LUMEN_VENV) then a project-local lumen_modules/. Lets installed packages
    // resolve by bare name without changing the sibling-import behavior above.
    for root in module_search_roots() {
        let mut p = root;
        for s in &segs {
            p.push(s);
        }
        p.set_extension("lm");
        if p.exists() {
            return (p, access);
        }
    }

    // Nothing matched: return the sibling path so the existing "cannot read
    // module" error points at the most expected location.
    (sib, access)
}

// Package search roots, highest precedence first: active venv, then a
// project-local lumen_modules/. Mirrors pkg::modules_dir but lists all roots.
fn module_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(v) = std::env::var("LUMEN_VENV") {
        if !v.is_empty() {
            roots.push(Path::new(&v).join("lumen_modules"));
        }
    }
    roots.push(PathBuf::from("lumen_modules"));
    roots
}

pub fn collect(
    prog: &Program,
    base_dir: &Path,
    visited: &mut HashSet<String>,
    out: &mut Vec<Item>,
    aliases: &mut HashMap<String, String>,
    is_builtin: &dyn Fn(&str) -> bool,
) -> Result<(), crate::CompileError> {
    for item in prog {
        let Item::Import(imp) = item else { continue };
        let module = &imp.module;
        if is_builtin(module) {

            continue;
        }
        let (path, access) = module_to_path(base_dir, module);

        let access_name = imp.alias.clone().unwrap_or(access);
        aliases.insert(access_name, module.clone());

        let key = path.to_string_lossy().to_string();
        // Dedup by resolved path so a diamond of imports loads each module once
        // and recursive/cyclic imports terminate.
        if !visited.insert(key) {
            continue;
        }
        let src = std::fs::read_to_string(&path).map_err(|e| {
            crate::CompileError::Import(format!(
                "cannot read module '{}' ({}): {}",
                module,
                path.display(),
                e
            ))
        })?;
        let sub = crate::parse_program(&src)?;

        let sub_dir = path.parent().unwrap_or(base_dir).to_path_buf();
        let mut sub_aliases = HashMap::new();
        collect(&sub, &sub_dir, visited, out, &mut sub_aliases, is_builtin)?;
        let start = out.len();
        for it in &sub {
            match it {
                Item::Fn(_) | Item::Struct(_) | Item::ExternBlock(_) => out.push(it.clone()),
                Item::Import(_) | Item::Stmt(_) => {}
            }
        }
        rewrite_program(&sub_aliases, is_builtin, &mut out[start..]);
    }
    Ok(())
}

pub fn rewrite_program(
    aliases: &HashMap<String, String>,
    is_builtin: &dyn Fn(&str) -> bool,
    items: &mut [Item],
) {
    for it in items.iter_mut() {
        match it {
            Item::Fn(f) => rewrite_block(aliases, is_builtin, &mut f.body),
            Item::Struct(s) => {
                for m in &mut s.methods {
                    rewrite_block(aliases, is_builtin, &mut m.body);
                }
            }
            Item::Stmt(s) => rewrite_stmt(aliases, is_builtin, s),
            Item::Import(_) | Item::ExternBlock(_) => {}
        }
    }
}

fn rewrite_block(
    aliases: &HashMap<String, String>,
    is_builtin: &dyn Fn(&str) -> bool,
    body: &mut [Stmt],
) {
    for s in body.iter_mut() {
        rewrite_stmt(aliases, is_builtin, s);
    }
}

fn rewrite_stmt(
    aliases: &HashMap<String, String>,
    is_builtin: &dyn Fn(&str) -> bool,
    s: &mut Stmt,
) {
    match s {
        Stmt::Let { value, .. } => rewrite_expr(aliases, is_builtin, value),
        Stmt::Assign { target, value } => {
            rewrite_expr(aliases, is_builtin, target);
            rewrite_expr(aliases, is_builtin, value);
        }
        Stmt::ExprStmt(e) | Stmt::Return(Some(e)) => rewrite_expr(aliases, is_builtin, e),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => {}
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            rewrite_expr(aliases, is_builtin, cond);
            rewrite_block(aliases, is_builtin, then);
            for (c, b) in elifs.iter_mut() {
                rewrite_expr(aliases, is_builtin, c);
                rewrite_block(aliases, is_builtin, b);
            }
            if let Some(b) = els {
                rewrite_block(aliases, is_builtin, b);
            }
        }
        Stmt::While { cond, body } => {
            rewrite_expr(aliases, is_builtin, cond);
            rewrite_block(aliases, is_builtin, body);
        }
        Stmt::For { iter, body, .. } => {
            rewrite_expr(aliases, is_builtin, iter);
            rewrite_block(aliases, is_builtin, body);
        }
        Stmt::Try {
            body, catch_body, ..
        } => {
            rewrite_block(aliases, is_builtin, body);
            rewrite_block(aliases, is_builtin, catch_body);
        }
        Stmt::Raise(e) => rewrite_expr(aliases, is_builtin, e),
    }
}

fn rewrite_expr(
    aliases: &HashMap<String, String>,
    is_builtin: &dyn Fn(&str) -> bool,
    e: &mut Expr,
) {

    // `alias.foo(args)` parses as a method call, but if `alias` is an imported
    // module (not a builtin) it's really a qualified function call. Rewrite it to
    // a plain Call to `foo`, since imports share one flat namespace.
    if let Expr::Method { obj, name, args } = e {
        if let Expr::Ident(m) = obj.as_ref() {
            let is_module_alias = aliases.contains_key(m) && !is_builtin(m);
            if is_module_alias {
                let mut new_args = std::mem::take(args);
                for a in new_args.iter_mut() {
                    rewrite_expr(aliases, is_builtin, a);
                }
                *e = Expr::Call {
                    callee: Box::new(Expr::Ident(name.clone())),
                    args: new_args,
                };
                return;
            }
        }
    }

    // Same idea for the field form `alias.foo`: if `alias` is an imported module,
    // hand back the field name so the call/field site below can drop the qualifier.
    let module_field = |obj: &Expr| -> Option<String> {
        if let Expr::Field { obj: inner, name } = obj {
            if let Expr::Ident(m) = inner.as_ref() {
                if aliases.contains_key(m) && !is_builtin(m) {
                    return Some(name.clone());
                }
            }
        }
        None
    };
    match e {
        Expr::NamedCall { callee, args } => {
            if let Some(fname) = module_field(callee) {
                **callee = Expr::Ident(fname);
            }
            rewrite_expr(aliases, is_builtin, callee);
            for (_, a) in args.iter_mut() {
                rewrite_expr(aliases, is_builtin, a);
            }
            return;
        }
        Expr::Call { callee, args } => {
            if let Some(fname) = module_field(callee) {
                **callee = Expr::Ident(fname);
            }
            rewrite_expr(aliases, is_builtin, callee);
            for a in args.iter_mut() {
                rewrite_expr(aliases, is_builtin, a);
            }
            return;
        }
        Expr::Field { .. } => {
            if let Some(fname) = module_field(e) {
                *e = Expr::Ident(fname);
                return;
            }
        }
        _ => {}
    }

    match e {
        Expr::Unary { expr, .. } => rewrite_expr(aliases, is_builtin, expr),
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr(aliases, is_builtin, lhs);
            rewrite_expr(aliases, is_builtin, rhs);
        }
        Expr::Call { callee, args } => {
            rewrite_expr(aliases, is_builtin, callee);
            for a in args.iter_mut() {
                rewrite_expr(aliases, is_builtin, a);
            }
        }
        Expr::NamedCall { callee, args } => {
            rewrite_expr(aliases, is_builtin, callee);
            for (_, a) in args.iter_mut() {
                rewrite_expr(aliases, is_builtin, a);
            }
        }
        Expr::Method { obj, args, .. } => {
            rewrite_expr(aliases, is_builtin, obj);
            for a in args.iter_mut() {
                rewrite_expr(aliases, is_builtin, a);
            }
        }
        Expr::Field { obj, .. } => rewrite_expr(aliases, is_builtin, obj),
        Expr::Index { obj, index } => {
            rewrite_expr(aliases, is_builtin, obj);
            rewrite_expr(aliases, is_builtin, index);
        }
        Expr::Range { lo, hi } => {
            rewrite_expr(aliases, is_builtin, lo);
            rewrite_expr(aliases, is_builtin, hi);
        }
        Expr::List(xs) => {
            for x in xs.iter_mut() {
                rewrite_expr(aliases, is_builtin, x);
            }
        }
        Expr::Map(pairs) => {
            for (k, v) in pairs.iter_mut() {
                rewrite_expr(aliases, is_builtin, k);
                rewrite_expr(aliases, is_builtin, v);
            }
        }
        Expr::Lambda { body, .. } => rewrite_block(aliases, is_builtin, body),
        Expr::FStr(parts) => {
            for p in parts.iter_mut() {
                if let FStrPart::Expr(x) = p {
                    rewrite_expr(aliases, is_builtin, x);
                }
            }
        }
        _ => {}
    }
}
