//! Lambda lifting. Hoists every lambda into a top-level fn named `__lambda_N`,
//! turning its free variables into leading parameters and the lambda site into
//! a Closure (or a bare fn reference when there are no captures). The subtle
//! bit is the capture strategy: a variable that is both captured AND mutated is
//! "celled" (boxed in a one-element list) so the closure and its enclosing scope
//! share one mutable slot; everything else is captured by value.
use crate::ast::*;
use std::collections::HashSet;

pub fn lift_program(prog: &mut Program) {
    let mut globals: HashSet<String> = HashSet::new();
    for item in prog.iter() {
        if let Item::Fn(f) = item {
            globals.insert(f.name.clone());
        }
    }

    let mut ctx = Lifter {
        counter: 0,
        generated: Vec::new(),
        globals,
    };

    for item in prog.iter_mut() {
        match item {
            Item::Fn(f) => ctx.process_unit(&mut f.body),
            Item::Struct(s) => {
                for m in s.methods.iter_mut() {
                    ctx.process_unit(&mut m.body);
                }
            }
            Item::Stmt(_) | Item::ExternBlock(_) | Item::Import(_) => {}
        }
    }

    let mut top_refs: Vec<&mut Stmt> = prog
        .iter_mut()
        .filter_map(|it| match it {
            Item::Stmt(s) => Some(s),
            _ => None,
        })
        .collect();
    if !top_refs.is_empty() {
        ctx.proc_refs(&mut top_refs);
    }

    for f in ctx.generated.drain(..) {
        prog.push(Item::Fn(f));
    }
}

struct Lifter {
    counter: u32,
    generated: Vec<FnDef>,
    globals: HashSet<String>,
}

impl Lifter {
    fn fresh_name(&mut self) -> String {
        let n = self.counter;
        self.counter += 1;
        format!("__lambda_{}", n)
    }

    #[allow(clippy::ptr_arg)]
    fn process_unit(&mut self, body: &mut Vec<Stmt>) {
        let celled = self.compute_celled(body);
        if !celled.is_empty() {
            for s in body.iter_mut() {
                cell_stmt(s, &celled);
            }
        }
        for s in body.iter_mut() {
            self.lift_stmt(s, &celled);
        }
    }

    fn proc_refs(&mut self, body: &mut [&mut Stmt]) {
        let celled = {
            let mut captured = HashSet::new();
            let mut assigned = HashSet::new();
            for s in body.iter() {
                captured_stmt(s, &self.globals, &mut captured);
                is_assigned(s, &mut assigned);
            }
            captured
                .intersection(&assigned)
                .cloned()
                .collect::<HashSet<String>>()
        };
        if !celled.is_empty() {
            for s in body.iter_mut() {
                cell_stmt(s, &celled);
            }
        }
        for s in body.iter_mut() {
            self.lift_stmt(s, &celled);
        }
    }

    // A variable needs a cell exactly when it's both captured by some lambda and
    // assigned somewhere in this scope. Captured-but-never-mutated stays by value.
    fn compute_celled(&self, body: &[Stmt]) -> HashSet<String> {
        let mut captured = HashSet::new();
        let mut assigned = HashSet::new();
        for s in body {
            captured_stmt(s, &self.globals, &mut captured);
            is_assigned(s, &mut assigned);
        }
        captured.intersection(&assigned).cloned().collect()
    }

    fn lift_block(&mut self, body: &mut [Stmt], celled: &HashSet<String>) {
        for s in body.iter_mut() {
            self.lift_stmt(s, celled);
        }
    }

    fn lift_stmt(&mut self, s: &mut Stmt, celled: &HashSet<String>) {
        match s {
            Stmt::Let { value, .. } => self.lift_expr(value, celled),
            Stmt::Assign { target, value } => {
                self.lift_expr(target, celled);
                self.lift_expr(value, celled);
            }
            Stmt::ExprStmt(e) => self.lift_expr(e, celled),
            Stmt::Return(Some(e)) => self.lift_expr(e, celled),
            Stmt::Return(None) => {}
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } => {
                self.lift_expr(cond, celled);
                self.lift_block(then, celled);
                for (c, b) in elifs.iter_mut() {
                    self.lift_expr(c, celled);
                    self.lift_block(b, celled);
                }
                if let Some(b) = els {
                    self.lift_block(b, celled);
                }
            }
            Stmt::While { cond, body } => {
                self.lift_expr(cond, celled);
                self.lift_block(body, celled);
            }
            Stmt::For { iter, body, .. } => {
                self.lift_expr(iter, celled);
                self.lift_block(body, celled);
            }
            Stmt::Try {
                body, catch_body, ..
            } => {
                self.lift_block(body, celled);
                self.lift_block(catch_body, celled);
            }
            Stmt::Raise(e) => self.lift_expr(e, celled),
            Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => {}
        }
    }

    fn lift_expr(&mut self, e: &mut Expr, celled: &HashSet<String>) {
        match e {
            Expr::Unary { expr, .. } => self.lift_expr(expr, celled),
            Expr::Binary { lhs, rhs, .. } => {
                self.lift_expr(lhs, celled);
                self.lift_expr(rhs, celled);
            }
            Expr::Call { callee, args } => {
                self.lift_expr(callee, celled);
                for a in args.iter_mut() {
                    self.lift_expr(a, celled);
                }
            }
            Expr::NamedCall { callee, args } => {
                self.lift_expr(callee, celled);
                for (_, a) in args.iter_mut() {
                    self.lift_expr(a, celled);
                }
            }
            Expr::Method { obj, args, .. } => {
                self.lift_expr(obj, celled);
                for a in args.iter_mut() {
                    self.lift_expr(a, celled);
                }
            }
            Expr::Field { obj, .. } => self.lift_expr(obj, celled),
            Expr::Index { obj, index } => {
                self.lift_expr(obj, celled);
                self.lift_expr(index, celled);
            }
            Expr::List(xs) => {
                for x in xs.iter_mut() {
                    self.lift_expr(x, celled);
                }
            }
            Expr::Map(kvs) => {
                for (k, v) in kvs.iter_mut() {
                    self.lift_expr(k, celled);
                    self.lift_expr(v, celled);
                }
            }
            Expr::Range { lo, hi } => {
                self.lift_expr(lo, celled);
                self.lift_expr(hi, celled);
            }
            Expr::FStr(parts) => {
                for p in parts.iter_mut() {
                    if let FStrPart::Expr(pe) = p {
                        self.lift_expr(pe, celled);
                    }
                }
            }
            Expr::Lambda { body, .. } => self.lift_block(body, celled),
            Expr::ListComp {
                elem, iter, cond, ..
            } => {
                // A lambda can sit in a comprehension's elem/cond s- notably the
                // desugared `xs.map(f)`/`xs.filter(f)`. Recurse so it gets lifted.
                self.lift_expr(elem, celled);
                self.lift_expr(iter, celled);
                if let Some(c) = cond {
                    self.lift_expr(c, celled);
                }
            }
            _ => {}
        }

        // Children handled above; now lift this lambda itself if it is one.
        // Free vars = identifiers used but not bound by params or globals. They
        // become the closure's captures and the lifted fn's leading params.
        if let Expr::Lambda { params, body } = e {
            let params = std::mem::take(params);
            let body: Vec<Stmt> = std::mem::take(body);

            let mut bound: HashSet<String> = params.iter().cloned().collect();
            bound.extend(self.globals.iter().cloned());
            let mut captures: Vec<String> = Vec::new();
            fv_block(&body, &bound, &mut captures);

            let mut lifted_body = body;
            let local_cells: HashSet<String> = captures
                .iter()
                .filter(|c| celled.contains(*c))
                .cloned()
                .collect();
            if !local_cells.is_empty() {
                for s in lifted_body.iter_mut() {
                    cell_stmt(s, &local_cells);
                }
            }

            let name = self.fresh_name();
            let mut fn_params: Vec<Param> = captures
                .iter()
                .map(|c| Param {
                    name: c.clone(),
                    ty: Type::Dynamic,
                    default: None,
                })
                .collect();
            fn_params.extend(params.into_iter().map(|p| Param {
                name: p,
                ty: Type::Dynamic,
                default: None,
            }));

            let def = FnDef {
                name: name.clone(),
                params: fn_params,
                ret: Type::Unknown,
                body: lifted_body,
                exported: false,
                is_method: false,
            };
            self.generated.push(def);

            if captures.is_empty() {
                // No captures: just a named top-level fn, refer to it by name.
                *e = Expr::Ident(name);
            } else {
                *e = Expr::Closure {
                    fn_name: name,
                    captures: captures.into_iter().map(Expr::Ident).collect(),
                };
            }
        }
    }
}

// A celled variable lives as the single element of a one-element list, so every
// read/write of `x` becomes `x[0]`. This gives the closure and outer scope a
// shared, mutable box without a dedicated cell type in the runtime.
fn cell_index(name: &str) -> Expr {
    Expr::Index {
        obj: Box::new(Expr::Ident(name.to_string())),
        index: Box::new(Expr::Int(0)),
    }
}

fn cell_stmt(s: &mut Stmt, celled: &HashSet<String>) {
    match s {
        Stmt::Let { name, value, .. } => {
            cell_expr(value, celled);
            if celled.contains(name) {
                let init = std::mem::replace(value, Expr::Nil);
                *value = Expr::List(vec![init]);
            }
        }
        Stmt::Assign { target, value } => {
            cell_expr(value, celled);

            if let Expr::Ident(n) = target {
                if celled.contains(n) {
                    *target = cell_index(n);
                    return;
                }
            }
            cell_expr(target, celled);
        }
        Stmt::ExprStmt(e) => cell_expr(e, celled),
        Stmt::Return(Some(e)) => cell_expr(e, celled),
        Stmt::Return(None) => {}
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            cell_expr(cond, celled);
            for s in then.iter_mut() {
                cell_stmt(s, celled);
            }
            for (c, b) in elifs.iter_mut() {
                cell_expr(c, celled);
                for s in b.iter_mut() {
                    cell_stmt(s, celled);
                }
            }
            if let Some(b) = els {
                for s in b.iter_mut() {
                    cell_stmt(s, celled);
                }
            }
        }
        Stmt::While { cond, body } => {
            cell_expr(cond, celled);
            for s in body.iter_mut() {
                cell_stmt(s, celled);
            }
        }
        Stmt::For { iter, body, .. } => {
            cell_expr(iter, celled);
            for s in body.iter_mut() {
                cell_stmt(s, celled);
            }
        }
        Stmt::Try {
            body, catch_body, ..
        } => {
            for s in body.iter_mut() {
                cell_stmt(s, celled);
            }
            for s in catch_body.iter_mut() {
                cell_stmt(s, celled);
            }
        }
        Stmt::Raise(e) => cell_expr(e, celled),
        Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => {}
    }
}

fn cell_expr(e: &mut Expr, celled: &HashSet<String>) {
    match e {
        Expr::Ident(n) => {
            if celled.contains(n) {
                *e = cell_index(n);
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Nil
        | Expr::SelfExpr => {}
        Expr::Unary { expr, .. } => cell_expr(expr, celled),
        Expr::Binary { lhs, rhs, .. } => {
            cell_expr(lhs, celled);
            cell_expr(rhs, celled);
        }
        Expr::Call { callee, args } => {
            cell_expr(callee, celled);
            for a in args.iter_mut() {
                cell_expr(a, celled);
            }
        }
        Expr::NamedCall { callee, args } => {
            cell_expr(callee, celled);
            for (_, a) in args.iter_mut() {
                cell_expr(a, celled);
            }
        }
        Expr::Method { obj, args, .. } => {
            cell_expr(obj, celled);
            for a in args.iter_mut() {
                cell_expr(a, celled);
            }
        }
        Expr::Field { obj, .. } => cell_expr(obj, celled),
        Expr::Index { obj, index } => {
            cell_expr(obj, celled);
            cell_expr(index, celled);
        }
        Expr::List(xs) => {
            for x in xs.iter_mut() {
                cell_expr(x, celled);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs.iter_mut() {
                cell_expr(k, celled);
                cell_expr(v, celled);
            }
        }
        Expr::Range { lo, hi } => {
            cell_expr(lo, celled);
            cell_expr(hi, celled);
        }
        Expr::IfElse { cond, then, els } => {
            cell_expr(cond, celled);
            cell_expr(then, celled);
            cell_expr(els, celled);
        }
        Expr::ListComp {
            elem, iter, cond, ..
        } => {
            cell_expr(elem, celled);
            cell_expr(iter, celled);
            if let Some(c) = cond {
                cell_expr(c, celled);
            }
        }
        Expr::Slice { obj, lo, hi } => {
            cell_expr(obj, celled);
            if let Some(lo) = lo {
                cell_expr(lo, celled);
            }
            if let Some(hi) = hi {
                cell_expr(hi, celled);
            }
        }
        Expr::FStr(parts) => {
            for p in parts.iter_mut() {
                if let FStrPart::Expr(pe) = p {
                    cell_expr(pe, celled);
                }
            }
        }

        Expr::Closure { captures, .. } => {
            for c in captures.iter_mut() {
                if let Expr::Ident(n) = c {
                    if celled.contains(n) {
                        continue;
                    }
                }
                cell_expr(c, celled);
            }
        }

        Expr::Lambda { .. } => {}
    }
}

fn captured_stmt(s: &Stmt, globals: &HashSet<String>, out: &mut HashSet<String>) {
    match s {
        Stmt::Let { value, .. } => captured_expr(value, globals, out),
        Stmt::Assign { target, value } => {
            captured_expr(target, globals, out);
            captured_expr(value, globals, out);
        }
        Stmt::ExprStmt(e) | Stmt::Return(Some(e)) => captured_expr(e, globals, out),
        Stmt::Return(None) => {}
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            captured_expr(cond, globals, out);
            for s in then {
                captured_stmt(s, globals, out);
            }
            for (c, b) in elifs {
                captured_expr(c, globals, out);
                for s in b {
                    captured_stmt(s, globals, out);
                }
            }
            if let Some(b) = els {
                for s in b {
                    captured_stmt(s, globals, out);
                }
            }
        }
        Stmt::While { cond, body } => {
            captured_expr(cond, globals, out);
            for s in body {
                captured_stmt(s, globals, out);
            }
        }
        Stmt::For { iter, body, .. } => {
            captured_expr(iter, globals, out);
            for s in body {
                captured_stmt(s, globals, out);
            }
        }
        Stmt::Try {
            body, catch_body, ..
        } => {
            for s in body {
                captured_stmt(s, globals, out);
            }
            for s in catch_body {
                captured_stmt(s, globals, out);
            }
        }
        Stmt::Raise(e) => captured_expr(e, globals, out),
        Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => {}
    }
}

fn captured_expr(e: &Expr, globals: &HashSet<String>, out: &mut HashSet<String>) {
    match e {
        Expr::Lambda { params, body } => {
            let mut bound: HashSet<String> = params.iter().cloned().collect();
            bound.extend(globals.iter().cloned());
            let mut fv = Vec::new();
            fv_block(body, &bound, &mut fv);
            out.extend(fv);

            for s in body {
                captured_stmt(s, globals, out);
            }
        }
        Expr::Unary { expr, .. } => captured_expr(expr, globals, out),
        Expr::Binary { lhs, rhs, .. } => {
            captured_expr(lhs, globals, out);
            captured_expr(rhs, globals, out);
        }
        Expr::Call { callee, args } => {
            captured_expr(callee, globals, out);
            for a in args {
                captured_expr(a, globals, out);
            }
        }
        Expr::NamedCall { callee, args } => {
            captured_expr(callee, globals, out);
            for (_, a) in args {
                captured_expr(a, globals, out);
            }
        }
        Expr::Method { obj, args, .. } => {
            captured_expr(obj, globals, out);
            for a in args {
                captured_expr(a, globals, out);
            }
        }
        Expr::Field { obj, .. } => captured_expr(obj, globals, out),
        Expr::Index { obj, index } => {
            captured_expr(obj, globals, out);
            captured_expr(index, globals, out);
        }
        Expr::List(xs) => {
            for x in xs {
                captured_expr(x, globals, out);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs {
                captured_expr(k, globals, out);
                captured_expr(v, globals, out);
            }
        }
        Expr::Range { lo, hi } => {
            captured_expr(lo, globals, out);
            captured_expr(hi, globals, out);
        }
        Expr::FStr(parts) => {
            for p in parts {
                if let FStrPart::Expr(pe) = p {
                    captured_expr(pe, globals, out);
                }
            }
        }
        Expr::Closure { captures, .. } => {
            for c in captures {
                captured_expr(c, globals, out);
            }
        }
        _ => {}
    }
}

fn is_assigned(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::Let { value, .. } => assigned_expr(value, out),
        Stmt::Assign { target, value } => {
            if let Expr::Ident(n) = target {
                out.insert(n.clone());
            }
            assigned_expr(target, out);
            assigned_expr(value, out);
        }
        Stmt::ExprStmt(e) | Stmt::Return(Some(e)) => assigned_expr(e, out),
        Stmt::Return(None) => {}
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            assigned_expr(cond, out);
            for s in then {
                is_assigned(s, out);
            }
            for (c, b) in elifs {
                assigned_expr(c, out);
                for s in b {
                    is_assigned(s, out);
                }
            }
            if let Some(b) = els {
                for s in b {
                    is_assigned(s, out);
                }
            }
        }
        Stmt::While { cond, body } => {
            assigned_expr(cond, out);
            for s in body {
                is_assigned(s, out);
            }
        }
        Stmt::For { var, iter, body } => {
            let _ = var;
            assigned_expr(iter, out);
            for s in body {
                is_assigned(s, out);
            }
        }
        Stmt::Try {
            body, catch_body, ..
        } => {
            for s in body {
                is_assigned(s, out);
            }
            for s in catch_body {
                is_assigned(s, out);
            }
        }
        Stmt::Raise(e) => assigned_expr(e, out),
        Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => {}
    }
}

fn assigned_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Lambda { body, .. } => {
            for s in body {
                is_assigned(s, out);
            }
        }
        Expr::Unary { expr, .. } => assigned_expr(expr, out),
        Expr::Binary { lhs, rhs, .. } => {
            assigned_expr(lhs, out);
            assigned_expr(rhs, out);
        }
        Expr::Call { callee, args } => {
            assigned_expr(callee, out);
            for a in args {
                assigned_expr(a, out);
            }
        }
        Expr::NamedCall { callee, args } => {
            assigned_expr(callee, out);
            for (_, a) in args {
                assigned_expr(a, out);
            }
        }
        Expr::Method { obj, args, .. } => {
            assigned_expr(obj, out);
            for a in args {
                assigned_expr(a, out);
            }
        }
        Expr::Field { obj, .. } => assigned_expr(obj, out),
        Expr::Index { obj, index } => {
            assigned_expr(obj, out);
            assigned_expr(index, out);
        }
        Expr::List(xs) => {
            for x in xs {
                assigned_expr(x, out);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs {
                assigned_expr(k, out);
                assigned_expr(v, out);
            }
        }
        Expr::Range { lo, hi } => {
            assigned_expr(lo, out);
            assigned_expr(hi, out);
        }
        Expr::FStr(parts) => {
            for p in parts {
                if let FStrPart::Expr(pe) = p {
                    assigned_expr(pe, out);
                }
            }
        }
        _ => {}
    }
}

fn free_vars(e: &Expr, bound: &HashSet<String>, out: &mut Vec<String>) {
    match e {
        Expr::Ident(n) => {
            if !bound.contains(n) && !out.contains(n) {
                out.push(n.clone());
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Nil
        | Expr::SelfExpr => {}
        Expr::Unary { expr, .. } => free_vars(expr, bound, out),
        Expr::Binary { lhs, rhs, .. } => {
            free_vars(lhs, bound, out);
            free_vars(rhs, bound, out);
        }
        Expr::Call { callee, args } => {
            free_vars(callee, bound, out);
            for a in args {
                free_vars(a, bound, out);
            }
        }
        Expr::NamedCall { callee, args } => {
            free_vars(callee, bound, out);
            for (_, a) in args {
                free_vars(a, bound, out);
            }
        }
        Expr::Method { obj, args, .. } => {
            free_vars(obj, bound, out);
            for a in args {
                free_vars(a, bound, out);
            }
        }
        Expr::Field { obj, .. } => free_vars(obj, bound, out),
        Expr::Index { obj, index } => {
            free_vars(obj, bound, out);
            free_vars(index, bound, out);
        }
        Expr::List(xs) => {
            for x in xs {
                free_vars(x, bound, out);
            }
        }
        Expr::Map(kvs) => {
            for (k, v) in kvs {
                free_vars(k, bound, out);
                free_vars(v, bound, out);
            }
        }
        Expr::Range { lo, hi } => {
            free_vars(lo, bound, out);
            free_vars(hi, bound, out);
        }
        Expr::IfElse { cond, then, els } => {
            free_vars(cond, bound, out);
            free_vars(then, bound, out);
            free_vars(els, bound, out);
        }
        Expr::ListComp {
            elem,
            var,
            iter,
            cond,
        } => {
            free_vars(iter, bound, out);
            let mut inner = bound.clone();
            inner.insert(var.clone());
            free_vars(elem, &inner, out);
            if let Some(c) = cond {
                free_vars(c, &inner, out);
            }
        }
        Expr::Slice { obj, lo, hi } => {
            free_vars(obj, bound, out);
            if let Some(lo) = lo {
                free_vars(lo, bound, out);
            }
            if let Some(hi) = hi {
                free_vars(hi, bound, out);
            }
        }
        Expr::FStr(parts) => {
            for p in parts {
                if let FStrPart::Expr(pe) = p {
                    free_vars(pe, bound, out);
                }
            }
        }
        Expr::Lambda { params, body } => {
            let mut inner = bound.clone();
            inner.extend(params.iter().cloned());
            fv_block(body, &inner, out);
        }
        Expr::Closure { captures, .. } => {
            for c in captures {
                free_vars(c, bound, out);
            }
        }
    }
}

fn fv_block(body: &[Stmt], bound: &HashSet<String>, out: &mut Vec<String>) {
    let mut inner = bound.clone();
    for s in body {
        fv_stmt(s, &mut inner, out);
    }
}

fn fv_stmt(s: &Stmt, bound: &mut HashSet<String>, out: &mut Vec<String>) {
    match s {
        Stmt::Let { name, value, .. } => {
            free_vars(value, bound, out);
            bound.insert(name.clone());
        }
        Stmt::Assign { target, value } => {
            free_vars(target, bound, out);
            free_vars(value, bound, out);
        }
        Stmt::ExprStmt(e) | Stmt::Return(Some(e)) => free_vars(e, bound, out),
        Stmt::Return(None) => {}
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            free_vars(cond, bound, out);
            fv_scoped(then, bound, out);
            for (c, b) in elifs {
                free_vars(c, bound, out);
                fv_scoped(b, bound, out);
            }
            if let Some(b) = els {
                fv_scoped(b, bound, out);
            }
        }
        Stmt::While { cond, body } => {
            free_vars(cond, bound, out);
            fv_scoped(body, bound, out);
        }
        Stmt::For { var, iter, body } => {
            free_vars(iter, bound, out);
            let mut inner = bound.clone();
            inner.insert(var.clone());
            for s in body {
                fv_stmt(s, &mut inner, out);
            }
        }
        Stmt::Try {
            body,
            catch_var,
            catch_body,
        } => {
            fv_scoped(body, bound, out);
            let mut inner = bound.clone();
            inner.insert(catch_var.clone());
            for s in catch_body {
                fv_stmt(s, &mut inner, out);
            }
        }
        Stmt::Raise(e) => free_vars(e, bound, out),
        Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => {}
    }
}

fn fv_scoped(body: &[Stmt], bound: &HashSet<String>, out: &mut Vec<String>) {
    let mut inner = bound.clone();
    for s in body {
        fv_stmt(s, &mut inner, out);
    }
}
