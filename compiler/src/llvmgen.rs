//! LLVM backend: lowers the merged AST to textual LLVM IR (.ll) that calls the
//! same lumen_* runtime ABI the asm backend (codegen.rs) uses. Because the
//! runtime is shared and every op routes through it, output is byte-identical
//! to the interpreter and the asm backend.
//!
//! MVP strategy: fully boxed. Every Lumen value is an i64 (NaN-boxed LumenVal).
//! Every operation calls a runtime helper (lumen_add, lumen_eq, lumen_print,
//! ...). No unboxing fast paths yet - correctness first, speed later. LLVM -O2
//! still optimizes the surrounding glue, and the runtime does the real work.

use crate::ast::*;
use std::collections::HashMap;
use std::fmt::Write;

pub struct LlvmGen {
    // module-level text built up as we go
    decls: String,   // declare lines (deduped via `declared`)
    globals: String, // private string/const globals
    body: String,    // all function definitions

    declared: std::collections::HashSet<String>,
    str_count: usize,
    tmp: usize, // SSA value counter, per fn (reset in gen_fn)
    lbl: usize, // label counter, per fn

    fns: HashMap<String, FnDef>,
    structs: HashMap<String, StructDef>,
    methods: HashMap<(String, String), FnDef>,
    externs: HashMap<String, ExternFn>,
    info: crate::types::IntInfo, // proven int/float verdicts for unboxing
}

// A local variable lives in an alloca; we load/store its i64 by name.
struct Ctx {
    locals: HashMap<String, String>, // lumen name -> llvm slot reg (%v.N)
    func: String,
    loops: Vec<(String, String)>, // (continue label, break label)
    self_slot: Option<String>,
    has_try: bool, // fn contains a try -> disable unboxing (setjmp hazard)
}

impl Default for LlvmGen {
    fn default() -> Self {
        Self::new()
    }
}

impl LlvmGen {
    pub fn new() -> Self {
        LlvmGen {
            decls: String::new(),
            globals: String::new(),
            body: String::new(),
            declared: std::collections::HashSet::new(),
            str_count: 0,
            tmp: 0,
            lbl: 0,
            fns: HashMap::new(),
            structs: HashMap::new(),
            methods: HashMap::new(),
            externs: HashMap::new(),
            info: crate::types::IntInfo::default(),
        }
    }

    fn sym(name: &str) -> String {
        format!("lm_{name}")
    }
    fn msym(s: &str, m: &str) -> String {
        format!("lm_{s}__{m}")
    }

    fn vreg(&mut self) -> String {
        self.tmp += 1;
        format!("%v{}", self.tmp)
    }
    fn label(&mut self, base: &str) -> String {
        self.lbl += 1;
        format!("{base}{}", self.lbl)
    }

    // Record a `declare` once. sig is the full text after `declare `.
    fn need(&mut self, sym: &str, sig: &str) {
        if self.declared.insert(sym.to_string()) {
            let _ = writeln!(self.decls, "declare {sig}");
        }
    }

    // Common runtime decls. Called lazily by helpers below.
    fn need_call(&mut self, sym: &str, nargs: usize) {
        let args = vec!["i64"; nargs].join(", ");
        self.need(sym, &format!("i64 @{sym}({args})"));
    }

    // Add a NUL-terminated string constant, return its ptr getelementptr expr.
    fn add_str(&mut self, s: &str) -> String {
        let (enc, n) = encode_cstr(s);
        let g = format!("@.str{}", self.str_count);
        self.str_count += 1;
        let _ = writeln!(
            self.globals,
            "{g} = private unnamed_addr constant [{n} x i8] c\"{enc}\""
        );
        format!("getelementptr inbounds [{n} x i8], ptr {g}, i64 0, i64 0")
    }

    pub fn generate(&mut self, prog: &Program) -> Result<String, String> {
        self.info = crate::types::analyze(prog);
        // collect decls (source order kept for determinism)
        for item in prog {
            match item {
                Item::Fn(f) => {
                    self.fns.insert(f.name.clone(), f.clone());
                }
                Item::Struct(s) => {
                    let e = self
                        .structs
                        .entry(s.name.clone())
                        .or_insert_with(|| StructDef {
                            name: s.name.clone(),
                            fields: Vec::new(),
                            methods: Vec::new(),
                        });
                    if !s.fields.is_empty() {
                        e.fields = s.fields.clone();
                    }
                    for m in &s.methods {
                        self.methods
                            .insert((s.name.clone(), m.name.clone()), m.clone());
                    }
                }
                Item::ExternBlock(b) => {
                    for ef in &b.fns {
                        self.externs.insert(ef.name.clone(), ef.clone());
                    }
                }
                _ => {}
            }
        }

        // top-level stmts become the entry fn body
        let mut top: Vec<Stmt> = Vec::new();
        for item in prog {
            if let Item::Stmt(s) = item {
                top.push(s.clone());
            }
        }

        // Pre-seed `declared` with every symbol we DEFINE in this module so a
        // later need() never emits a colliding `declare` for them (a function
        // cannot be both declared and defined in one module).
        for f in prog.iter().filter_map(|it| match it {
            Item::Fn(f) => Some(f),
            _ => None,
        }) {
            self.declared.insert(Self::sym(&f.name));
        }
        for ((sname, _), f) in self
            .methods
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>()
        {
            self.declared.insert(Self::msym(&sname, &f.name));
        }
        self.declared.insert("lumen_user_main".into());
        let main_called = top.iter().any(|s| {
            matches!(s, Stmt::ExprStmt(Expr::Call { callee, .. })
                if matches!(&**callee, Expr::Ident(n) if n == "main"))
        });
        if !main_called && self.fns.contains_key("main") {
            top.push(Stmt::ExprStmt(Expr::Call {
                callee: Box::new(Expr::Ident("main".into())),
                args: Vec::new(),
            }));
        }

        // emit user fns in source order
        let fns: Vec<FnDef> = prog
            .iter()
            .filter_map(|it| match it {
                Item::Fn(f) => Some(f.clone()),
                _ => None,
            })
            .collect();
        for f in &fns {
            let def = self.gen_fn(&Self::sym(&f.name), f, None)?;
            self.body.push_str(&def);
        }
        let methods: Vec<((String, String), FnDef)> = self
            .methods
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for ((sname, _), f) in &methods {
            let def = self.gen_fn(&Self::msym(sname, &f.name), f, Some(sname.clone()))?;
            self.body.push_str(&def);
        }

        // entry fn (top-level)
        let entry = FnDef {
            name: "__lumen_entry__".into(),
            params: Vec::new(),
            ret: Type::Unknown,
            body: top,
            exported: false,
            is_method: false,
        };
        let edef = self.gen_fn("lumen_user_main", &entry, None)?;
        self.body.push_str(&edef);

        // real C main: set args, init gc with stack base, run, exit 0
        self.need("lumen_set_args", "void @lumen_set_args(i32, ptr)");
        self.need("lumen_gc_init", "void @lumen_gc_init(ptr)");
        self.need("lumen_user_main", "i64 @lumen_user_main()");
        self.need("llvm.frameaddress.p0", "ptr @llvm.frameaddress.p0(i32)");
        let mut m = String::new();
        m.push_str("define i32 @main(i32 %argc, ptr %argv) {\nentry:\n");
        m.push_str("  call void @lumen_set_args(i32 %argc, ptr %argv)\n");
        m.push_str("  %fb = call ptr @llvm.frameaddress.p0(i32 0)\n");
        m.push_str("  call void @lumen_gc_init(ptr %fb)\n");
        m.push_str("  %_r = call i64 @lumen_user_main()\n");
        m.push_str("  ret i32 0\n}\n");
        self.body.push_str(&m);

        // assemble module
        let mut out = String::new();
        out.push_str("; Lumen LLVM backend output\n");
        out.push_str("target triple = \"x86_64-w64-windows-gnu\"\n\n");
        out.push_str(&self.decls);
        out.push('\n');
        out.push_str(&self.globals);
        out.push('\n');
        out.push_str(&self.body);
        Ok(out)
    }

    fn gen_fn(
        &mut self,
        sym: &str,
        f: &FnDef,
        struct_hint: Option<String>,
    ) -> Result<String, String> {
        self.tmp = 0;
        self.lbl = 0;
        let mut ctx = Ctx {
            locals: HashMap::new(),
            func: f.name.clone(),
            loops: Vec::new(),
            self_slot: None,
            has_try: body_has_try(&f.body),
        };

        // signature: i64 (i64 %a0, ...). `self` is already a regular param named
        // "self" in f.params for methods (the parser puts it there), so we do NOT
        // add an extra one - just lower every param uniformly.
        let mut params: Vec<String> = Vec::new();
        let mut prologue = String::new();
        let _ = struct_hint;
        for (i, p) in f.params.iter().enumerate() {
            params.push(format!("i64 %a{i}"));
            let slot = format!("%p{i}.slot");
            let _ = writeln!(prologue, "  {slot} = alloca i64");
            let _ = writeln!(prologue, "  store i64 %a{i}, ptr {slot}");
            ctx.locals.insert(p.name.clone(), slot.clone());
            if p.name == "self" {
                ctx.self_slot = Some(slot);
            }
        }

        let sig = params.join(", ");
        let mut b = String::new();
        let _ = writeln!(b, "define i64 @{sym}({sig}) {{");
        b.push_str("entry:\n");
        b.push_str(&prologue);

        let mut bodytext = String::new();
        let mut terminated = false;
        for st in &f.body {
            self.gen_stmt(st, &mut ctx, &mut bodytext, &mut terminated)?;
            if terminated {
                break;
            }
        }
        // Hoist every `alloca` out of the body into the entry block. LLVM requires
        // an alloca to dominate all its uses; emitting them lazily inside branch
        // blocks breaks that. Our allocas never depend on runtime values, so
        // moving them to entry is always safe.
        let mut hoisted = String::new();
        let mut rest = String::new();
        for line in bodytext.lines() {
            if line.trim_start().contains(" = alloca ") {
                hoisted.push_str(line);
                hoisted.push('\n');
            } else {
                rest.push_str(line);
                rest.push('\n');
            }
        }
        // setjmp hazard: our custom lumen_longjmp restores callee-saved regs +
        // rsp, so any value LLVM keeps in a register across the setjmp is stale
        // after a catch. The C-standard fix is `volatile` on locals live across
        // setjmp. We apply it to every slot load/store in functions that contain
        // a try - this blocks mem2reg from promoting them into clobbered regs.
        // Functions with no try are untouched, so they keep full -O2 speed.
        if body_has_try(&f.body) {
            rest = rest
                .replace("  store i64 ", "  store volatile i64 ")
                .replace(" = load i64, ptr ", " = load volatile i64, ptr ");
        }
        b.push_str(&hoisted);
        b.push_str(&rest);
        if !terminated {
            // implicit `return nil`
            self.need("lumen_nil", "i64 @lumen_nil()");
            let n = self.vreg();
            let _ = writeln!(b, "  {n} = call i64 @lumen_nil()");
            let _ = writeln!(b, "  ret i64 {n}");
        }
        b.push_str("}\n\n");
        Ok(b)
    }

    // ---- statements ----
    fn gen_stmt(
        &mut self,
        s: &Stmt,
        ctx: &mut Ctx,
        out: &mut String,
        term: &mut bool,
    ) -> Result<(), String> {
        match s {
            Stmt::SrcLine(n) => {
                // store directly to the runtime's line global instead of calling
                // lumen_set_line - a call is an optimization barrier on every
                // statement. Matches the asm backend (mov [rip+lumen_current_line]).
                // In try-functions we keep the CALL: a bare store can be sunk or
                // reordered across the custom setjmp/longjmp, scrambling which
                // line a fault reports. The call is opaque, so it stays put.
                if ctx.has_try {
                    self.need("lumen_set_line", "void @lumen_set_line(i32)");
                    let _ = writeln!(out, "  call void @lumen_set_line(i32 {n})");
                } else {
                    if self.declared.insert("@lumen_current_line".into()) {
                        self.decls
                            .push_str("@lumen_current_line = external global i32\n");
                    }
                    let _ = writeln!(out, "  store i32 {n}, ptr @lumen_current_line");
                }
            }
            Stmt::Let { name, value, .. } => {
                let v = self.gen_expr(value, ctx, out)?;
                let slot = match ctx.locals.get(name) {
                    Some(s) => s.clone(),
                    None => {
                        let s = format!("%l.{}", sanitize(name));
                        let _ = writeln!(out, "  {s} = alloca i64");
                        ctx.locals.insert(name.clone(), s.clone());
                        s
                    }
                };
                let _ = writeln!(out, "  store i64 {v}, ptr {slot}");
            }
            Stmt::Assign { target, value } => {
                let v = self.gen_expr(value, ctx, out)?;
                self.gen_assign(target, &v, ctx, out)?;
            }
            Stmt::ExprStmt(e) => {
                let _ = self.gen_expr(e, ctx, out)?;
            }
            Stmt::Return(opt) => {
                let v = match opt {
                    Some(e) => self.gen_expr(e, ctx, out)?,
                    None => {
                        self.need("lumen_nil", "i64 @lumen_nil()");
                        let n = self.vreg();
                        let _ = writeln!(out, "  {n} = call i64 @lumen_nil()");
                        n
                    }
                };
                let _ = writeln!(out, "  ret i64 {v}");
                *term = true;
            }
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } => {
                self.gen_if(cond, then, elifs, els, ctx, out, term)?;
            }
            Stmt::While { cond, body } => {
                let top = self.label("wtop");
                let bodyl = self.label("wbody");
                let end = self.label("wend");
                let _ = writeln!(out, "  br label %{top}");
                let _ = writeln!(out, "{top}:");
                let c = self.gen_truthy(cond, ctx, out)?;
                let _ = writeln!(out, "  br i1 {c}, label %{bodyl}, label %{end}");
                let _ = writeln!(out, "{bodyl}:");
                ctx.loops.push((top.clone(), end.clone()));
                let mut t = false;
                for st in body {
                    self.gen_stmt(st, ctx, out, &mut t)?;
                    if t {
                        break;
                    }
                }
                ctx.loops.pop();
                if !t {
                    let _ = writeln!(out, "  br label %{top}");
                }
                let _ = writeln!(out, "{end}:");
            }
            Stmt::For { var, iter, body } => {
                if let Expr::Range { lo, hi } = iter {
                    self.gen_for_range(var, lo, hi, body, ctx, out)?;
                } else {
                    self.gen_for_list(var, iter, body, ctx, out)?;
                }
            }
            Stmt::Break => {
                if let Some((_, end)) = ctx.loops.last() {
                    let _ = writeln!(out, "  br label %{end}");
                    *term = true;
                } else {
                    return Err("break outside loop".into());
                }
            }
            Stmt::Continue => {
                if let Some((cont, _)) = ctx.loops.last() {
                    let _ = writeln!(out, "  br label %{cont}");
                    *term = true;
                } else {
                    return Err("continue outside loop".into());
                }
            }
            Stmt::Raise(e) => {
                let v = self.gen_expr(e, ctx, out)?;
                // mirror asm: stringify the raised value before raising, so
                // `raise 404` becomes the message "404" (lumen_cstr only reads
                // OBJ_STR; a raw int would yield an empty message otherwise).
                self.need("lumen_to_str", "i64 @lumen_to_str(i64)");
                self.need("lumen_raise", "i64 @lumen_raise(i64)");
                let s = self.vreg();
                let _ = writeln!(out, "  {s} = call i64 @lumen_to_str(i64 {v})");
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @lumen_raise(i64 {s})");
            }
            Stmt::Try {
                body,
                catch_var,
                catch_body,
            } => {
                self.gen_try(body, catch_var, catch_body, ctx, out, term)?;
            }
        }
        Ok(())
    }

    fn gen_if(
        &mut self,
        cond: &Expr,
        then: &[Stmt],
        elifs: &[(Expr, Vec<Stmt>)],
        els: &Option<Vec<Stmt>>,
        ctx: &mut Ctx,
        out: &mut String,
        term: &mut bool,
    ) -> Result<(), String> {
        // Build a chain: cond -> then; elif... ; else
        let end = self.label("ifend");
        let mut branches: Vec<(&Expr, &[Stmt])> = vec![(cond, then)];
        for (c, b) in elifs {
            branches.push((c, b));
        }
        let mut all_term = true;
        for (i, (c, b)) in branches.iter().enumerate() {
            let thenl = self.label("then");
            let nextl = self.label("next");
            let cv = self.gen_truthy(c, ctx, out)?;
            let _ = writeln!(out, "  br i1 {cv}, label %{thenl}, label %{nextl}");
            let _ = writeln!(out, "{thenl}:");
            let mut t = false;
            for st in *b {
                self.gen_stmt(st, ctx, out, &mut t)?;
                if t {
                    break;
                }
            }
            if !t {
                let _ = writeln!(out, "  br label %{end}");
                all_term = false;
            }
            let _ = writeln!(out, "{nextl}:");
            let _ = i;
        }
        // else
        match els {
            Some(b) => {
                let mut t = false;
                for st in b {
                    self.gen_stmt(st, ctx, out, &mut t)?;
                    if t {
                        break;
                    }
                }
                if !t {
                    let _ = writeln!(out, "  br label %{end}");
                    all_term = false;
                } else {
                    all_term = all_term && true;
                }
            }
            None => {
                let _ = writeln!(out, "  br label %{end}");
                all_term = false;
            }
        }
        let _ = writeln!(out, "{end}:");
        // only mark terminated if every branch + else terminated AND there is no
        // fallthrough end use - we keep `end:` labeled so emit a no-op landing.
        // Conservatively never mark terminated (end: is reachable label).
        let _ = all_term;
        let _ = term;
        Ok(())
    }

    fn gen_for_range(
        &mut self,
        var: &str,
        lo: &Expr,
        hi: &Expr,
        body: &[Stmt],
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<(), String> {
        // i = lo; while i < hi: var = box(i); body; i += 1
        // values stay boxed; we unbox via lumen_to_int for the counter.
        self.need("lumen_to_int", "i64 @lumen_to_int(i64)");
        self.need("lumen_from_int", "i64 @lumen_from_int(i64)");
        let lov = self.gen_expr(lo, ctx, out)?;
        let hiv = self.gen_expr(hi, ctx, out)?;
        let ci = self.vreg();
        let _ = writeln!(out, "  {ci} = call i64 @lumen_to_int(i64 {lov})");
        let hiu = self.vreg();
        let _ = writeln!(out, "  {hiu} = call i64 @lumen_to_int(i64 {hiv})");
        // counter + limit in allocas (simple, correct; LLVM promotes to regs)
        let cslot = self.vreg();
        let _ = writeln!(out, "  {cslot} = alloca i64");
        let _ = writeln!(out, "  store i64 {ci}, ptr {cslot}");
        let lslot = self.vreg();
        let _ = writeln!(out, "  {lslot} = alloca i64");
        let _ = writeln!(out, "  store i64 {hiu}, ptr {lslot}");
        // var slot
        let vslot = match ctx.locals.get(var) {
            Some(s) => s.clone(),
            None => {
                let s = format!("%l.{}", sanitize(var));
                let _ = writeln!(out, "  {s} = alloca i64");
                ctx.locals.insert(var.into(), s.clone());
                s
            }
        };
        let top = self.label("ftop");
        let bodyl = self.label("fbody");
        let cont = self.label("fcont");
        let end = self.label("fend");
        let _ = writeln!(out, "  br label %{top}");
        let _ = writeln!(out, "{top}:");
        let curi = self.vreg();
        let _ = writeln!(out, "  {curi} = load i64, ptr {cslot}");
        let limi = self.vreg();
        let _ = writeln!(out, "  {limi} = load i64, ptr {lslot}");
        let cmp = self.vreg();
        let _ = writeln!(out, "  {cmp} = icmp slt i64 {curi}, {limi}");
        let _ = writeln!(out, "  br i1 {cmp}, label %{bodyl}, label %{end}");
        let _ = writeln!(out, "{bodyl}:");
        let boxed = self.vreg();
        let _ = writeln!(out, "  {boxed} = call i64 @lumen_from_int(i64 {curi})");
        let _ = writeln!(out, "  store i64 {boxed}, ptr {vslot}");
        // skip the GC poll when the body provably can't allocate (tight numeric loop)
        if self.stmts_alloc(body, ctx) {
            self.gc_poll(out);
        }
        ctx.loops.push((cont.clone(), end.clone()));
        let mut t = false;
        for st in body {
            self.gen_stmt(st, ctx, out, &mut t)?;
            if t {
                break;
            }
        }
        ctx.loops.pop();
        if !t {
            let _ = writeln!(out, "  br label %{cont}");
        }
        let _ = writeln!(out, "{cont}:");
        let cur2 = self.vreg();
        let _ = writeln!(out, "  {cur2} = load i64, ptr {cslot}");
        let inc = self.vreg();
        let _ = writeln!(out, "  {inc} = add i64 {cur2}, 1");
        let _ = writeln!(out, "  store i64 {inc}, ptr {cslot}");
        let _ = writeln!(out, "  br label %{top}");
        let _ = writeln!(out, "{end}:");
        Ok(())
    }

    fn gen_for_list(
        &mut self,
        var: &str,
        iter: &Expr,
        body: &[Stmt],
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<(), String> {
        self.need("lumen_iter_prep", "i64 @lumen_iter_prep(i64)");
        self.need("lumen_len", "i64 @lumen_len(i64)");
        self.need("lumen_list_get", "i64 @lumen_list_get(i64, i64)");
        let it = self.gen_expr(iter, ctx, out)?;
        let lst = self.vreg();
        let _ = writeln!(out, "  {lst} = call i64 @lumen_iter_prep(i64 {it})");
        let lslot = self.vreg();
        let _ = writeln!(out, "  {lslot} = alloca i64");
        let _ = writeln!(out, "  store i64 {lst}, ptr {lslot}");
        let len = self.vreg();
        let _ = writeln!(out, "  {len} = call i64 @lumen_len(i64 {lst})");
        let lenslot = self.vreg();
        let _ = writeln!(out, "  {lenslot} = alloca i64");
        let _ = writeln!(out, "  store i64 {len}, ptr {lenslot}");
        let islot = self.vreg();
        let _ = writeln!(out, "  {islot} = alloca i64");
        let _ = writeln!(out, "  store i64 0, ptr {islot}");
        let vslot = match ctx.locals.get(var) {
            Some(s) => s.clone(),
            None => {
                let s = format!("%l.{}", sanitize(var));
                let _ = writeln!(out, "  {s} = alloca i64");
                ctx.locals.insert(var.into(), s.clone());
                s
            }
        };
        let top = self.label("ltop");
        let bodyl = self.label("lbody");
        let cont = self.label("lcont");
        let end = self.label("lend");
        let _ = writeln!(out, "  br label %{top}");
        let _ = writeln!(out, "{top}:");
        let i = self.vreg();
        let _ = writeln!(out, "  {i} = load i64, ptr {islot}");
        let ln = self.vreg();
        let _ = writeln!(out, "  {ln} = load i64, ptr {lenslot}");
        let cmp = self.vreg();
        let _ = writeln!(out, "  {cmp} = icmp slt i64 {i}, {ln}");
        let _ = writeln!(out, "  br i1 {cmp}, label %{bodyl}, label %{end}");
        let _ = writeln!(out, "{bodyl}:");
        let l2 = self.vreg();
        let _ = writeln!(out, "  {l2} = load i64, ptr {lslot}");
        let el = self.vreg();
        let _ = writeln!(out, "  {el} = call i64 @lumen_list_get(i64 {l2}, i64 {i})");
        let _ = writeln!(out, "  store i64 {el}, ptr {vslot}");
        if self.stmts_alloc(body, ctx) {
            self.gc_poll(out);
        }
        ctx.loops.push((cont.clone(), end.clone()));
        let mut t = false;
        for st in body {
            self.gen_stmt(st, ctx, out, &mut t)?;
            if t {
                break;
            }
        }
        ctx.loops.pop();
        if !t {
            let _ = writeln!(out, "  br label %{cont}");
        }
        let _ = writeln!(out, "{cont}:");
        let i2 = self.vreg();
        let _ = writeln!(out, "  {i2} = load i64, ptr {islot}");
        let inc = self.vreg();
        let _ = writeln!(out, "  {inc} = add i64 {i2}, 1");
        let _ = writeln!(out, "  store i64 {inc}, ptr {islot}");
        let _ = writeln!(out, "  br label %{top}");
        let _ = writeln!(out, "{end}:");
        Ok(())
    }

    fn gen_try(
        &mut self,
        body: &[Stmt],
        catch_var: &str,
        catch_body: &[Stmt],
        ctx: &mut Ctx,
        out: &mut String,
        _term: &mut bool,
    ) -> Result<(), String> {
        // mirror asm: buf = lumen_try_push(); r = lumen_setjmp(buf); if r: catch
        // else body + pop. lumen_setjmp is a custom naked setjmp, so it MUST be
        // marked returns_twice or LLVM will move code illegally across it. All
        // locals live in allocas (and we reload on every read), so values stay
        // valid after the longjmp lands in the catch block.
        self.need("lumen_try_push", "ptr @lumen_try_push()");
        self.need("lumen_try_pop", "void @lumen_try_pop()");
        self.need("lumen_setjmp", "i32 @lumen_setjmp(ptr) returns_twice");
        self.need("lumen_caught_msg", "i64 @lumen_caught_msg()");
        let catchl = self.label("catch");
        let bodyl = self.label("trybody");
        let end = self.label("tryend");
        let buf = self.vreg();
        let _ = writeln!(out, "  {buf} = call ptr @lumen_try_push()");
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = call i32 @lumen_setjmp(ptr {buf})");
        let nz = self.vreg();
        let _ = writeln!(out, "  {nz} = icmp ne i32 {r}, 0");
        let _ = writeln!(out, "  br i1 {nz}, label %{catchl}, label %{bodyl}");
        // body path
        let _ = writeln!(out, "{bodyl}:");
        let mut t = false;
        for st in body {
            self.gen_stmt(st, ctx, out, &mut t)?;
            if t {
                break;
            }
        }
        if !t {
            let _ = writeln!(out, "  call void @lumen_try_pop()");
            let _ = writeln!(out, "  br label %{end}");
        }
        // catch path: bind msg, run catch body
        let _ = writeln!(out, "{catchl}:");
        let msg = self.vreg();
        let _ = writeln!(out, "  {msg} = call i64 @lumen_caught_msg()");
        let slot = match ctx.locals.get(catch_var) {
            Some(s) => s.clone(),
            None => {
                let s = format!("%l.{}", sanitize(catch_var));
                let _ = writeln!(out, "  {s} = alloca i64");
                ctx.locals.insert(catch_var.into(), s.clone());
                s
            }
        };
        let _ = writeln!(out, "  store i64 {msg}, ptr {slot}");
        let mut t2 = false;
        for st in catch_body {
            self.gen_stmt(st, ctx, out, &mut t2)?;
            if t2 {
                break;
            }
        }
        if !t2 {
            let _ = writeln!(out, "  br label %{end}");
        }
        let _ = writeln!(out, "{end}:");
        Ok(())
    }

    // assignment target: ident | index | field
    fn gen_assign(
        &mut self,
        target: &Expr,
        v: &str,
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<(), String> {
        match target {
            Expr::Ident(n) => {
                let slot = match ctx.locals.get(n) {
                    Some(s) => s.clone(),
                    None => {
                        let s = format!("%l.{}", sanitize(n));
                        let _ = writeln!(out, "  {s} = alloca i64");
                        ctx.locals.insert(n.clone(), s.clone());
                        s
                    }
                };
                let _ = writeln!(out, "  store i64 {v}, ptr {slot}");
            }
            Expr::Index { obj, index } => {
                self.need("lumen_index_set", "void @lumen_index_set(i64, i64, i64)");
                let o = self.gen_expr(obj, ctx, out)?;
                let k = self.gen_expr(index, ctx, out)?;
                let _ = writeln!(
                    out,
                    "  call void @lumen_index_set(i64 {o}, i64 {k}, i64 {v})"
                );
            }
            Expr::Field { obj, name } => {
                self.need("lumen_struct_set", "void @lumen_struct_set(i64, ptr, i64)");
                let o = self.gen_expr(obj, ctx, out)?;
                let nm = self.add_str(name);
                let p = self.vreg();
                let _ = writeln!(out, "  {p} = {nm}");
                let _ = writeln!(
                    out,
                    "  call void @lumen_struct_set(i64 {o}, ptr {p}, i64 {v})"
                );
            }
            _ => return Err("invalid assignment target".into()),
        }
        Ok(())
    }

    // GC safepoint: if allocs_since_gc >= 512 call collect. Roots: the MVP keeps
    // all live values in allocas, so the conservative C-stack scan still sees
    // them (the allocas live in this frame). Precise shadow-stack is the next
    // step (plan doc 03); this is correct because nothing is kept only in a
    // register across the call - allocas are memory.
    // Conservative: can running these stmts allocate heap (bump allocs_since_gc)?
    // Only heap objects (str/list/map/struct/closure) and calls allocate; unboxed
    // int/float arithmetic does not. Used to skip the GC poll in tight numeric
    // loops. Defaults to TRUE for anything uncertain (safe: an extra poll is fine).
    fn stmts_alloc(&self, body: &[Stmt], ctx: &Ctx) -> bool {
        body.iter().any(|s| self.stmt_allocs(s, ctx))
    }
    fn stmt_allocs(&self, s: &Stmt, ctx: &Ctx) -> bool {
        match s {
            Stmt::SrcLine(_) | Stmt::Break | Stmt::Continue => false,
            Stmt::Let { value, .. } => self.expr_allocs(value, ctx),
            Stmt::Assign { value, .. } => self.expr_allocs(value, ctx),
            Stmt::ExprStmt(e) => self.expr_allocs(e, ctx),
            Stmt::Return(Some(e)) => self.expr_allocs(e, ctx),
            Stmt::Return(None) => false,
            Stmt::If { cond, then, elifs, els } => {
                self.expr_allocs(cond, ctx)
                    || self.stmts_alloc(then, ctx)
                    || elifs
                        .iter()
                        .any(|(c, b)| self.expr_allocs(c, ctx) || self.stmts_alloc(b, ctx))
                    || els.as_ref().is_some_and(|b| self.stmts_alloc(b, ctx))
            }
            // nested loops/try: assume they may allocate (keep their own polls)
            _ => true,
        }
    }
    fn expr_allocs(&self, e: &Expr, ctx: &Ctx) -> bool {
        match e {
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Nil
            | Expr::Ident(_) | Expr::SelfExpr => false,
            Expr::Unary { expr, .. } => self.expr_allocs(expr, ctx),
            Expr::Binary { lhs, rhs, .. } => {
                // proven-numeric binops compute unboxed (no alloc); otherwise the
                // boxed path could be a string concat etc. -> assume it allocates.
                let numeric = (self.known_int(lhs, ctx) && self.known_int(rhs, ctx))
                    || (self.known_float(lhs, ctx) && self.known_float(rhs, ctx));
                !numeric || self.expr_allocs(lhs, ctx) || self.expr_allocs(rhs, ctx)
            }
            Expr::Index { obj, index } => self.expr_allocs(obj, ctx) || self.expr_allocs(index, ctx),
            Expr::Field { obj, .. } => self.expr_allocs(obj, ctx),
            // literals that build heap objects, calls, comprehensions, closures
            _ => true,
        }
    }

    fn gc_poll(&mut self, out: &mut String) {
        self.need("lumen_gc_collect", "void @lumen_gc_collect()");
        if self.declared.insert("@lumen_allocs_since_gc".into()) {
            self.decls
                .push_str("@lumen_allocs_since_gc = external global i64\n");
        }
        let c = self.vreg();
        let _ = writeln!(out, "  {c} = load i64, ptr @lumen_allocs_since_gc");
        let hit = self.vreg();
        let _ = writeln!(out, "  {hit} = icmp sge i64 {c}, 512");
        let g = self.label("gc");
        let sk = self.label("gcskip");
        let _ = writeln!(out, "  br i1 {hit}, label %{g}, label %{sk}");
        let _ = writeln!(out, "{g}:");
        let _ = writeln!(out, "  call void @lumen_gc_collect()");
        let _ = writeln!(out, "  br label %{sk}");
        let _ = writeln!(out, "{sk}:");
    }

    // ---- expressions: every result is an i64 ssa value ----
    fn gen_expr(&mut self, e: &Expr, ctx: &mut Ctx, out: &mut String) -> Result<String, String> {
        match e {
            Expr::Int(n) => {
                self.need("lumen_from_int", "i64 @lumen_from_int(i64)");
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @lumen_from_int(i64 {n})");
                Ok(r)
            }
            Expr::Float(f) => {
                self.need("lumen_from_double", "i64 @lumen_from_double(double)");
                let r = self.vreg();
                let _ = writeln!(
                    out,
                    "  {r} = call i64 @lumen_from_double(double {})",
                    fmt_double(*f)
                );
                Ok(r)
            }
            Expr::Bool(b) => {
                self.need("lumen_bool", "i64 @lumen_bool(i32)");
                let r = self.vreg();
                let _ = writeln!(
                    out,
                    "  {r} = call i64 @lumen_bool(i32 {})",
                    if *b { 1 } else { 0 }
                );
                Ok(r)
            }
            Expr::Nil => {
                self.need("lumen_nil", "i64 @lumen_nil()");
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @lumen_nil()");
                Ok(r)
            }
            Expr::Str(s) => {
                self.need("lumen_str_new", "i64 @lumen_str_new(ptr)");
                let g = self.add_str(s);
                let p = self.vreg();
                let _ = writeln!(out, "  {p} = {g}");
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @lumen_str_new(ptr {p})");
                Ok(r)
            }
            Expr::FStr(parts) => self.gen_fstring(parts, ctx, out),
            Expr::Ident(n) => {
                if let Some(slot) = ctx.locals.get(n) {
                    let r = self.vreg();
                    let _ = writeln!(out, "  {r} = load i64, ptr {slot}");
                    Ok(r)
                } else if self.fns.contains_key(n) {
                    // bare function reference -> closure with 0 captures
                    self.gen_closure(n, &[], ctx, out)
                } else {
                    Err(format!("undefined name '{n}'"))
                }
            }
            Expr::SelfExpr => {
                let slot = ctx.self_slot.clone().ok_or("self outside method")?;
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = load i64, ptr {slot}");
                Ok(r)
            }
            Expr::Unary { op, expr } => {
                let v = self.gen_expr(expr, ctx, out)?;
                match op {
                    UnOp::Neg => {
                        self.need("lumen_neg", "i64 @lumen_neg(i64)");
                        let r = self.vreg();
                        let _ = writeln!(out, "  {r} = call i64 @lumen_neg(i64 {v})");
                        Ok(r)
                    }
                    UnOp::Not => {
                        self.need("lumen_truthy", "i32 @lumen_truthy(i64)");
                        self.need("lumen_bool", "i64 @lumen_bool(i32)");
                        let t = self.vreg();
                        let _ = writeln!(out, "  {t} = call i32 @lumen_truthy(i64 {v})");
                        let z = self.vreg();
                        let _ = writeln!(out, "  {z} = icmp eq i32 {t}, 0");
                        let zi = self.vreg();
                        let _ = writeln!(out, "  {zi} = zext i1 {z} to i32");
                        let r = self.vreg();
                        let _ = writeln!(out, "  {r} = call i64 @lumen_bool(i32 {zi})");
                        Ok(r)
                    }
                }
            }
            Expr::Binary { op, lhs, rhs } => self.gen_binary(*op, lhs, rhs, ctx, out),
            Expr::IfElse { cond, then, els } => {
                // value-producing ternary via a slot + branches
                let slot = self.vreg();
                let _ = writeln!(out, "  {slot} = alloca i64");
                let c = self.gen_truthy(cond, ctx, out)?;
                let tl = self.label("tern_t");
                let fl = self.label("tern_f");
                let end = self.label("tern_e");
                let _ = writeln!(out, "  br i1 {c}, label %{tl}, label %{fl}");
                let _ = writeln!(out, "{tl}:");
                let tv = self.gen_expr(then, ctx, out)?;
                let _ = writeln!(out, "  store i64 {tv}, ptr {slot}");
                let _ = writeln!(out, "  br label %{end}");
                let _ = writeln!(out, "{fl}:");
                let fv = self.gen_expr(els, ctx, out)?;
                let _ = writeln!(out, "  store i64 {fv}, ptr {slot}");
                let _ = writeln!(out, "  br label %{end}");
                let _ = writeln!(out, "{end}:");
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = load i64, ptr {slot}");
                Ok(r)
            }
            Expr::List(xs) => {
                self.need("lumen_list_new", "i64 @lumen_list_new(i64)");
                self.need("lumen_list_push", "void @lumen_list_push(i64, i64)");
                let lst = self.vreg();
                let _ = writeln!(out, "  {lst} = call i64 @lumen_list_new(i64 {})", xs.len());
                let lslot = self.vreg();
                let _ = writeln!(out, "  {lslot} = alloca i64");
                let _ = writeln!(out, "  store i64 {lst}, ptr {lslot}");
                for x in xs {
                    let v = self.gen_expr(x, ctx, out)?;
                    let l = self.vreg();
                    let _ = writeln!(out, "  {l} = load i64, ptr {lslot}");
                    let _ = writeln!(out, "  call void @lumen_list_push(i64 {l}, i64 {v})");
                }
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = load i64, ptr {lslot}");
                Ok(r)
            }
            Expr::Map(kvs) => {
                self.need("lumen_map_new", "i64 @lumen_map_new()");
                self.need("lumen_map_set", "void @lumen_map_set(i64, i64, i64)");
                let m = self.vreg();
                let _ = writeln!(out, "  {m} = call i64 @lumen_map_new()");
                let mslot = self.vreg();
                let _ = writeln!(out, "  {mslot} = alloca i64");
                let _ = writeln!(out, "  store i64 {m}, ptr {mslot}");
                for (k, val) in kvs {
                    let kv = self.gen_expr(k, ctx, out)?;
                    let vv = self.gen_expr(val, ctx, out)?;
                    let mm = self.vreg();
                    let _ = writeln!(out, "  {mm} = load i64, ptr {mslot}");
                    let _ = writeln!(
                        out,
                        "  call void @lumen_map_set(i64 {mm}, i64 {kv}, i64 {vv})"
                    );
                }
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = load i64, ptr {mslot}");
                Ok(r)
            }
            Expr::Index { obj, index } => {
                self.need("lumen_index_get", "i64 @lumen_index_get(i64, i64)");
                let o = self.gen_expr(obj, ctx, out)?;
                let k = self.gen_expr(index, ctx, out)?;
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @lumen_index_get(i64 {o}, i64 {k})");
                Ok(r)
            }
            Expr::Slice { obj, lo, hi } => {
                self.need("lumen_slice", "i64 @lumen_slice(i64, i64, i64)");
                self.need("lumen_nil", "i64 @lumen_nil()");
                let o = self.gen_expr(obj, ctx, out)?;
                let lov = match lo {
                    Some(e) => self.gen_expr(e, ctx, out)?,
                    None => {
                        let n = self.vreg();
                        let _ = writeln!(out, "  {n} = call i64 @lumen_nil()");
                        n
                    }
                };
                let hiv = match hi {
                    Some(e) => self.gen_expr(e, ctx, out)?,
                    None => {
                        let n = self.vreg();
                        let _ = writeln!(out, "  {n} = call i64 @lumen_nil()");
                        n
                    }
                };
                let r = self.vreg();
                let _ = writeln!(
                    out,
                    "  {r} = call i64 @lumen_slice(i64 {o}, i64 {lov}, i64 {hiv})"
                );
                Ok(r)
            }
            Expr::Range { lo, hi } => {
                // range as a value -> build a list [lo, lo+1, .., hi-1]
                self.gen_range_list(lo, hi, ctx, out)
            }
            Expr::Field { obj, name } => {
                self.need("lumen_struct_get", "i64 @lumen_struct_get(i64, ptr)");
                let o = self.gen_expr(obj, ctx, out)?;
                let nm = self.add_str(name);
                let p = self.vreg();
                let _ = writeln!(out, "  {p} = {nm}");
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @lumen_struct_get(i64 {o}, ptr {p})");
                Ok(r)
            }
            Expr::Call { callee, args } => self.gen_call(callee, args, ctx, out),
            Expr::Method { obj, name, args } => self.gen_method(obj, name, args, ctx, out),
            Expr::Closure { fn_name, captures } => self.gen_closure(fn_name, captures, ctx, out),
            Expr::Lambda { .. } => Err("internal: lambda should be lifted before codegen".into()),
            Expr::NamedCall { callee, args } => self.gen_named_call(callee, args, ctx, out),
            Expr::ListComp {
                elem,
                var,
                iter,
                cond,
            } => self.gen_listcomp(elem, var, iter, cond, ctx, out),
        }
    }

    // List comprehension: lower to an accumulator list + a for loop that pushes
    // each (optionally filtered) element. Mirrors codegen.rs:1627 exactly so the
    // result is byte-identical.
    fn gen_listcomp(
        &mut self,
        elem: &Expr,
        var: &str,
        iter: &Expr,
        cond: &Option<Box<Expr>>,
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<String, String> {
        self.need("lumen_list_new", "i64 @lumen_list_new(i64)");
        let acc = format!("__lc_acc_{}", {
            self.tmp += 1;
            self.tmp
        });
        let lst = self.vreg();
        let _ = writeln!(out, "  {lst} = call i64 @lumen_list_new(i64 0)");
        let accslot = format!("%l.{}", sanitize(&acc));
        let _ = writeln!(out, "  {accslot} = alloca i64");
        let _ = writeln!(out, "  store i64 {lst}, ptr {accslot}");
        ctx.locals.insert(acc.clone(), accslot.clone());

        let push = Stmt::ExprStmt(Expr::Method {
            obj: Box::new(Expr::Ident(acc.clone())),
            name: "push".to_string(),
            args: vec![elem.clone()],
        });
        let body = match cond {
            Some(c) => vec![Stmt::If {
                cond: (**c).clone(),
                then: vec![push],
                elifs: vec![],
                els: None,
            }],
            None => vec![push],
        };
        let for_stmt = Stmt::For {
            var: var.to_string(),
            iter: iter.clone(),
            body,
        };
        let mut t = false;
        self.gen_stmt(&for_stmt, ctx, out, &mut t)?;
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = load i64, ptr {accslot}");
        Ok(r)
    }

    fn gen_truthy(&mut self, e: &Expr, ctx: &mut Ctx, out: &mut String) -> Result<String, String> {
        // fast path: a proven int/float comparison feeding a branch -> emit the
        // icmp/fcmp i1 directly, skipping the lumen_bool box + lumen_truthy unbox
        // round-trip. Big win for if/while conditions (e.g. fib's `n < 2`).
        if !ctx.has_try {
            if let Expr::Binary { op, lhs, rhs } = e {
                if is_cmp(*op) {
                    if self.known_int(lhs, ctx) && self.known_int(rhs, ctx) {
                        let a = self.eval_int(lhs, ctx, out)?;
                        let b = self.eval_int(rhs, ctx, out)?;
                        let r = self.vreg();
                        let _ = writeln!(out, "  {r} = icmp {} i64 {a}, {b}", icmp_cc(*op));
                        return Ok(r);
                    }
                    if self.known_float(lhs, ctx) && self.known_float(rhs, ctx) {
                        let a = self.eval_float(lhs, ctx, out)?;
                        let b = self.eval_float(rhs, ctx, out)?;
                        let r = self.vreg();
                        let _ = writeln!(out, "  {r} = fcmp {} double {a}, {b}", fcmp_cc(*op));
                        return Ok(r);
                    }
                }
            }
        }
        let v = self.gen_expr(e, ctx, out)?;
        self.need("lumen_truthy", "i32 @lumen_truthy(i64)");
        let t = self.vreg();
        let _ = writeln!(out, "  {t} = call i32 @lumen_truthy(i64 {v})");
        let b = self.vreg();
        let _ = writeln!(out, "  {b} = icmp ne i32 {t}, 0");
        Ok(b)
    }

    fn gen_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<String, String> {
        // short-circuit and/or, result is one of the operands (Lumen semantics)
        if op == BinOp::And || op == BinOp::Or {
            let slot = self.vreg();
            let _ = writeln!(out, "  {slot} = alloca i64");
            let l = self.gen_expr(lhs, ctx, out)?;
            let _ = writeln!(out, "  store i64 {l}, ptr {slot}");
            self.need("lumen_truthy", "i32 @lumen_truthy(i64)");
            let t = self.vreg();
            let _ = writeln!(out, "  {t} = call i32 @lumen_truthy(i64 {l})");
            let c = self.vreg();
            let _ = writeln!(out, "  {c} = icmp ne i32 {t}, 0");
            let rhsl = self.label("sc_rhs");
            let end = self.label("sc_end");
            if op == BinOp::And {
                // if l truthy -> eval rhs; else keep l
                let _ = writeln!(out, "  br i1 {c}, label %{rhsl}, label %{end}");
            } else {
                // or: if l truthy -> keep l; else eval rhs
                let _ = writeln!(out, "  br i1 {c}, label %{end}, label %{rhsl}");
            }
            let _ = writeln!(out, "{rhsl}:");
            let rv = self.gen_expr(rhs, ctx, out)?;
            let _ = writeln!(out, "  store i64 {rv}, ptr {slot}");
            let _ = writeln!(out, "  br label %{end}");
            let _ = writeln!(out, "{end}:");
            let r = self.vreg();
            let _ = writeln!(out, "  {r} = load i64, ptr {slot}");
            return Ok(r);
        }

        // unboxed int fast path: both sides proven int -> inline i64 math, no
        // runtime call. Box the result (compares -> bool). Mirrors the asm
        // backend; wrap48 keeps overflow identical. Skipped in try-functions:
        // the custom setjmp/longjmp can clobber raw SSA temps that live across
        // it, so there we stay fully boxed (runtime calls are opaque, safe).
        if !ctx.has_try && self.known_int(lhs, ctx) && self.known_int(rhs, ctx) {
            if let Some(res) = self.int_binop(op, lhs, rhs, ctx, out)? {
                return Ok(res);
            }
        }
        // unboxed float fast path: both sides proven float -> inline f64 math
        // (no fast-math flags, so IEEE-identical to the runtime). Box = bitcast.
        if !ctx.has_try && self.known_float(lhs, ctx) && self.known_float(rhs, ctx) {
            if let Some(res) = self.float_binop(op, lhs, rhs, ctx, out)? {
                return Ok(res);
            }
        }

        let l = self.gen_expr(lhs, ctx, out)?;
        let r = self.gen_expr(rhs, ctx, out)?;
        let f = runtime_op(op);
        self.need_call(f, 2);
        let res = self.vreg();
        let _ = writeln!(out, "  {res} = call i64 @{f}(i64 {l}, i64 {r})");
        Ok(res)
    }

    // Is e provably a plain i64 (so we can compute it unboxed)? Conservative:
    // int literals, proven-int idents/params, int-returning calls, and int ops
    // over int operands. Anything else -> false (stay boxed).
    fn known_int(&self, e: &Expr, ctx: &Ctx) -> bool {
        match e {
            Expr::Int(_) => true,
            Expr::Ident(n) => self.info.is_int_var(&ctx.func, n),
            Expr::Unary { op: UnOp::Neg, expr } => self.known_int(expr, ctx),
            Expr::Binary { op, lhs, rhs } => {
                // Div/Mod only when the divisor is a nonzero int literal: then
                // the runtime's zero-check is moot and C-truncating srem/sdiv
                // matches lumen_div/lumen_mod exactly. Otherwise stay boxed.
                let arith = matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul);
                let divmod = matches!(op, BinOp::Div | BinOp::Mod) && nonzero_lit(rhs);
                (arith || divmod) && self.known_int(lhs, ctx) && self.known_int(rhs, ctx)
            }
            Expr::Call { callee, .. } => {
                matches!(&**callee, Expr::Ident(n) if self.info.int_ret.contains(n))
            }
            _ => false,
        }
    }

    // Evaluate a proven-int expr to a RAW i64 (unboxed). Leaves unbox via
    // shift; arithmetic stays raw with wrap48 after add/sub/mul.
    fn eval_int(&mut self, e: &Expr, ctx: &mut Ctx, out: &mut String) -> Result<String, String> {
        match e {
            Expr::Int(n) => Ok(format!("{n}")),
            Expr::Unary { op: UnOp::Neg, expr } => {
                let a = self.eval_int(expr, ctx, out)?;
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = sub i64 0, {a}");
                Ok(self.wrap48(&r, out))
            }
            Expr::Binary { op, lhs, rhs }
                if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul)
                    && self.known_int(lhs, ctx)
                    && self.known_int(rhs, ctx) =>
            {
                let a = self.eval_int(lhs, ctx, out)?;
                let b = self.eval_int(rhs, ctx, out)?;
                let r = self.vreg();
                let inst = match op {
                    BinOp::Add => "add",
                    BinOp::Sub => "sub",
                    BinOp::Mul => "mul",
                    _ => unreachable!(),
                };
                let _ = writeln!(out, "  {r} = {inst} i64 {a}, {b}");
                Ok(self.wrap48(&r, out))
            }
            // div/mod by a nonzero int literal: C-truncating sdiv/srem matches
            // the runtime exactly; result stays in range so no wrap48 needed.
            Expr::Binary { op, lhs, rhs }
                if matches!(op, BinOp::Div | BinOp::Mod)
                    && nonzero_lit(rhs)
                    && self.known_int(lhs, ctx) =>
            {
                let a = self.eval_int(lhs, ctx, out)?;
                let b = self.eval_int(rhs, ctx, out)?;
                let r = self.vreg();
                let inst = if *op == BinOp::Div { "sdiv" } else { "srem" };
                let _ = writeln!(out, "  {r} = {inst} i64 {a}, {b}");
                Ok(r)
            }
            // any other proven-int expr (ident, call): eval boxed then unbox
            _ => {
                let v = self.gen_expr(e, ctx, out)?;
                Ok(self.unbox_int(&v, out))
            }
        }
    }

    // sign-extend the low 48 bits of a raw i64 (shl 16; ashr 16). Keeps Lumen's
    // 48-bit wrapping identical to the interpreter and asm backend.
    fn wrap48(&mut self, v: &str, out: &mut String) -> String {
        let s = self.vreg();
        let _ = writeln!(out, "  {s} = shl i64 {v}, 16");
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = ashr i64 {s}, 16");
        r
    }

    // unbox a NaN-boxed int word to a raw i64 (shl 16; ashr 16).
    fn unbox_int(&mut self, v: &str, out: &mut String) -> String {
        self.wrap48(v, out)
    }

    // re-box a raw i64 into a NaN-boxed int: (raw & 0xFFFFFFFFFFFF) | tag.
    fn box_int(&mut self, raw: &str, out: &mut String) -> String {
        let m = self.vreg();
        let _ = writeln!(out, "  {m} = and i64 {raw}, 281474976710655"); // (1<<48)-1
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = or i64 {m}, 9221401712017801216"); // 0x7FF9000000000000
        r
    }

    // both operands proven int: compute unboxed, box the result. Returns None
    // for ops we don't fast-path (Pow, In, NotIn) so the caller falls back.
    fn int_binop(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<Option<String>, String> {
        // arithmetic -> raw result, box it. Div/Mod included only when known_int
        // already proved the divisor is a nonzero literal (eval_int emits sdiv/srem).
        let divmod_ok = matches!(op, BinOp::Div | BinOp::Mod) && nonzero_lit(rhs);
        if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) || divmod_ok {
            let raw = self.eval_int(&Expr::Binary {
                op,
                lhs: Box::new(lhs.clone()),
                rhs: Box::new(rhs.clone()),
            }, ctx, out)?;
            return Ok(Some(self.box_int(&raw, out)));
        }
        // compares -> icmp on raw, box as bool
        let cc = match op {
            BinOp::Eq => "eq",
            BinOp::Ne => "ne",
            BinOp::Lt => "slt",
            BinOp::Le => "sle",
            BinOp::Gt => "sgt",
            BinOp::Ge => "sge",
            _ => return Ok(None), // Pow / In / NotIn: not fast-pathed
        };
        let a = self.eval_int(lhs, ctx, out)?;
        let b = self.eval_int(rhs, ctx, out)?;
        let c = self.vreg();
        let _ = writeln!(out, "  {c} = icmp {cc} i64 {a}, {b}");
        let z = self.vreg();
        let _ = writeln!(out, "  {z} = zext i1 {c} to i32");
        self.need("lumen_bool", "i64 @lumen_bool(i32)");
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = call i64 @lumen_bool(i32 {z})");
        Ok(Some(r))
    }

    // Is e provably an f64? Float literals, proven-float idents/params,
    // float-returning calls, float ops over float operands.
    fn known_float(&self, e: &Expr, ctx: &Ctx) -> bool {
        match e {
            Expr::Float(_) => true,
            Expr::Ident(n) => self.info.is_float_var(&ctx.func, n),
            Expr::Unary { op: UnOp::Neg, expr } => self.known_float(expr, ctx),
            Expr::Binary { op, lhs, rhs } => {
                matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div
                ) && self.known_float(lhs, ctx)
                    && self.known_float(rhs, ctx)
            }
            Expr::Call { callee, .. } => {
                matches!(&**callee, Expr::Ident(n) if self.info.float_ret.contains(n))
            }
            _ => false,
        }
    }

    // Evaluate a proven-float expr to a RAW double. A Lumen float value IS its
    // own f64 bits (lumen_from_double is a bitcast), so unbox = bitcast i64->f64.
    fn eval_float(&mut self, e: &Expr, ctx: &mut Ctx, out: &mut String) -> Result<String, String> {
        match e {
            Expr::Float(f) => Ok(fmt_double(*f)),
            Expr::Unary { op: UnOp::Neg, expr } => {
                let a = self.eval_float(expr, ctx, out)?;
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = fneg double {a}");
                Ok(r)
            }
            Expr::Binary { op, lhs, rhs }
                if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
                    && self.known_float(lhs, ctx)
                    && self.known_float(rhs, ctx) =>
            {
                let a = self.eval_float(lhs, ctx, out)?;
                let b = self.eval_float(rhs, ctx, out)?;
                let inst = match op {
                    BinOp::Add => "fadd",
                    BinOp::Sub => "fsub",
                    BinOp::Mul => "fmul",
                    BinOp::Div => "fdiv",
                    _ => unreachable!(),
                };
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = {inst} double {a}, {b}");
                Ok(r)
            }
            // ident / call: eval boxed then bitcast i64 -> double
            _ => {
                let v = self.gen_expr(e, ctx, out)?;
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = bitcast i64 {v} to double");
                Ok(r)
            }
        }
    }

    // box a raw double into a LumenVal: bitcast double -> i64 (a real double's
    // bits ARE the boxed value; QNAN bits are clear).
    fn box_float(&mut self, raw: &str, out: &mut String) -> String {
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = bitcast double {raw} to i64");
        r
    }

    // both operands proven float: compute unboxed, box the result. Compares
    // return a boxed bool. Returns None for Pow/Mod/In (not fast-pathed).
    fn float_binop(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<Option<String>, String> {
        if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div) {
            let raw = self.eval_float(&Expr::Binary {
                op,
                lhs: Box::new(lhs.clone()),
                rhs: Box::new(rhs.clone()),
            }, ctx, out)?;
            return Ok(Some(self.box_float(&raw, out)));
        }
        let cc = match op {
            BinOp::Eq => "oeq",
            BinOp::Ne => "one",
            BinOp::Lt => "olt",
            BinOp::Le => "ole",
            BinOp::Gt => "ogt",
            BinOp::Ge => "oge",
            _ => return Ok(None), // Pow / Mod / In: not fast-pathed
        };
        let a = self.eval_float(lhs, ctx, out)?;
        let b = self.eval_float(rhs, ctx, out)?;
        let c = self.vreg();
        let _ = writeln!(out, "  {c} = fcmp {cc} double {a}, {b}");
        let z = self.vreg();
        let _ = writeln!(out, "  {z} = zext i1 {c} to i32");
        self.need("lumen_bool", "i64 @lumen_bool(i32)");
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = call i64 @lumen_bool(i32 {z})");
        Ok(Some(r))
    }

    fn gen_fstring(
        &mut self,
        parts: &[FStrPart],
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<String, String> {
        self.need("lumen_str_new", "i64 @lumen_str_new(ptr)");
        self.need("lumen_str_concat", "i64 @lumen_str_concat(i64, i64)");
        self.need("lumen_to_str", "i64 @lumen_to_str(i64)");
        let empty = self.add_str("");
        let ep = self.vreg();
        let _ = writeln!(out, "  {ep} = {empty}");
        let acc0 = self.vreg();
        let _ = writeln!(out, "  {acc0} = call i64 @lumen_str_new(ptr {ep})");
        let accslot = self.vreg();
        let _ = writeln!(out, "  {accslot} = alloca i64");
        let _ = writeln!(out, "  store i64 {acc0}, ptr {accslot}");
        for p in parts {
            let piece = match p {
                FStrPart::Lit(l) => {
                    let g = self.add_str(l);
                    let pp = self.vreg();
                    let _ = writeln!(out, "  {pp} = {g}");
                    let s = self.vreg();
                    let _ = writeln!(out, "  {s} = call i64 @lumen_str_new(ptr {pp})");
                    s
                }
                FStrPart::Expr(e) => {
                    let v = self.gen_expr(e, ctx, out)?;
                    let s = self.vreg();
                    let _ = writeln!(out, "  {s} = call i64 @lumen_to_str(i64 {v})");
                    s
                }
            };
            let acc = self.vreg();
            let _ = writeln!(out, "  {acc} = load i64, ptr {accslot}");
            let cat = self.vreg();
            let _ = writeln!(
                out,
                "  {cat} = call i64 @lumen_str_concat(i64 {acc}, i64 {piece})"
            );
            let _ = writeln!(out, "  store i64 {cat}, ptr {accslot}");
        }
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = load i64, ptr {accslot}");
        Ok(r)
    }

    fn gen_range_list(
        &mut self,
        lo: &Expr,
        hi: &Expr,
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<String, String> {
        self.need("lumen_to_int", "i64 @lumen_to_int(i64)");
        self.need("lumen_from_int", "i64 @lumen_from_int(i64)");
        self.need("lumen_list_new", "i64 @lumen_list_new(i64)");
        self.need("lumen_list_push", "void @lumen_list_push(i64, i64)");
        let lov = self.gen_expr(lo, ctx, out)?;
        let hiv = self.gen_expr(hi, ctx, out)?;
        let lo_i = self.vreg();
        let _ = writeln!(out, "  {lo_i} = call i64 @lumen_to_int(i64 {lov})");
        let hi_i = self.vreg();
        let _ = writeln!(out, "  {hi_i} = call i64 @lumen_to_int(i64 {hiv})");
        let lst = self.vreg();
        let _ = writeln!(out, "  {lst} = call i64 @lumen_list_new(i64 0)");
        let lslot = self.vreg();
        let _ = writeln!(out, "  {lslot} = alloca i64");
        let _ = writeln!(out, "  store i64 {lst}, ptr {lslot}");
        let islot = self.vreg();
        let _ = writeln!(out, "  {islot} = alloca i64");
        let _ = writeln!(out, "  store i64 {lo_i}, ptr {islot}");
        let limslot = self.vreg();
        let _ = writeln!(out, "  {limslot} = alloca i64");
        let _ = writeln!(out, "  store i64 {hi_i}, ptr {limslot}");
        let top = self.label("rtop");
        let bodyl = self.label("rbody");
        let end = self.label("rend");
        let _ = writeln!(out, "  br label %{top}");
        let _ = writeln!(out, "{top}:");
        let i = self.vreg();
        let _ = writeln!(out, "  {i} = load i64, ptr {islot}");
        let lim = self.vreg();
        let _ = writeln!(out, "  {lim} = load i64, ptr {limslot}");
        let cmp = self.vreg();
        let _ = writeln!(out, "  {cmp} = icmp slt i64 {i}, {lim}");
        let _ = writeln!(out, "  br i1 {cmp}, label %{bodyl}, label %{end}");
        let _ = writeln!(out, "{bodyl}:");
        let bx = self.vreg();
        let _ = writeln!(out, "  {bx} = call i64 @lumen_from_int(i64 {i})");
        let ll = self.vreg();
        let _ = writeln!(out, "  {ll} = load i64, ptr {lslot}");
        let _ = writeln!(out, "  call void @lumen_list_push(i64 {ll}, i64 {bx})");
        let i2 = self.vreg();
        let _ = writeln!(out, "  {i2} = add i64 {i}, 1");
        let _ = writeln!(out, "  store i64 {i2}, ptr {islot}");
        let _ = writeln!(out, "  br label %{top}");
        let _ = writeln!(out, "{end}:");
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = load i64, ptr {lslot}");
        Ok(r)
    }

    fn gen_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<String, String> {
        let name = match callee {
            Expr::Ident(n) => n.clone(),
            other => return self.gen_indirect(other, args, ctx, out),
        };
        // local var holding a callable -> indirect
        if ctx.locals.contains_key(&name) && !self.fns.contains_key(&name) {
            return self.gen_indirect(callee, args, ctx, out);
        }

        // builtins
        match name.as_str() {
            "print" => {
                if args.is_empty() {
                    self.need("lumen_print_nl", "void @lumen_print_nl()");
                    self.need("lumen_nil", "i64 @lumen_nil()");
                    let _ = writeln!(out, "  call void @lumen_print_nl()");
                    let r = self.vreg();
                    let _ = writeln!(out, "  {r} = call i64 @lumen_nil()");
                    return Ok(r);
                }
                if args.len() == 1 {
                    self.need("lumen_print", "void @lumen_print(i64)");
                    self.need("lumen_nil", "i64 @lumen_nil()");
                    let v = self.gen_expr(&args[0], ctx, out)?;
                    let _ = writeln!(out, "  call void @lumen_print(i64 {v})");
                    let r = self.vreg();
                    let _ = writeln!(out, "  {r} = call i64 @lumen_nil()");
                    return Ok(r);
                }
                self.need("lumen_print_part", "void @lumen_print_part(i64)");
                self.need("lumen_print_space", "void @lumen_print_space()");
                self.need("lumen_print_nl", "void @lumen_print_nl()");
                self.need("lumen_nil", "i64 @lumen_nil()");
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        let _ = writeln!(out, "  call void @lumen_print_space()");
                    }
                    let v = self.gen_expr(a, ctx, out)?;
                    let _ = writeln!(out, "  call void @lumen_print_part(i64 {v})");
                }
                let _ = writeln!(out, "  call void @lumen_print_nl()");
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @lumen_nil()");
                return Ok(r);
            }
            "len" => {
                self.need("lumen_len", "i64 @lumen_len(i64)");
                self.need("lumen_from_int", "i64 @lumen_from_int(i64)");
                let v = self.gen_expr(&args[0], ctx, out)?;
                let n = self.vreg();
                let _ = writeln!(out, "  {n} = call i64 @lumen_len(i64 {v})");
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @lumen_from_int(i64 {n})");
                return Ok(r);
            }
            "str" | "int" | "float" => {
                let sym = match name.as_str() {
                    "str" => "lumen_to_str",
                    "int" => "lumen_to_int_val",
                    _ => "lumen_to_float_val",
                };
                self.need_call(sym, 1);
                let v = self.gen_expr(&args[0], ctx, out)?;
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @{sym}(i64 {v})");
                return Ok(r);
            }
            "range" => {
                if args.len() == 1 {
                    return self.gen_range_list(&Expr::Int(0), &args[0], ctx, out);
                } else if args.len() == 2 {
                    return self.gen_range_list(&args[0], &args[1], ctx, out);
                }
                return Err("range() takes 1 or 2 args".into());
            }
            "sum" | "min" | "max" | "abs" | "round" | "type" | "ord" | "chr" | "is_digit"
            | "is_alpha" | "is_space" => {
                let sym = match name.as_str() {
                    "sum" => "lumen_sum",
                    "min" => "lumen_min",
                    "max" => "lumen_max",
                    "abs" => "lumen_abs",
                    "round" => "lumen_round",
                    "type" => "lumen_type",
                    "ord" => "lumen_ord",
                    "chr" => "lumen_chr",
                    "is_digit" => "lumen_is_digit",
                    "is_alpha" => "lumen_is_alpha",
                    _ => "lumen_is_space",
                };
                self.need_call(sym, 1);
                let v = self.gen_expr(&args[0], ctx, out)?;
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @{sym}(i64 {v})");
                return Ok(r);
            }
            "input" => {
                self.need_call("lumen_input", 1);
                self.need("lumen_nil", "i64 @lumen_nil()");
                let v = match args.first() {
                    Some(a) => self.gen_expr(a, ctx, out)?,
                    None => {
                        let n = self.vreg();
                        let _ = writeln!(out, "  {n} = call i64 @lumen_nil()");
                        n
                    }
                };
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @lumen_input(i64 {v})");
                return Ok(r);
            }
            "assert" => {
                self.need("lumen_assert", "void @lumen_assert(i64)");
                self.need("lumen_nil", "i64 @lumen_nil()");
                let v = self.gen_expr(&args[0], ctx, out)?;
                let _ = writeln!(out, "  call void @lumen_assert(i64 {v})");
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @lumen_nil()");
                return Ok(r);
            }
            "drop" => {
                self.need("lumen_release", "void @lumen_release(i64)");
                self.need("lumen_nil", "i64 @lumen_nil()");
                let v = self.gen_expr(&args[0], ctx, out)?;
                let _ = writeln!(out, "  call void @lumen_release(i64 {v})");
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @lumen_nil()");
                return Ok(r);
            }
            _ => {}
        }

        // struct constructor
        if self.structs.contains_key(&name) {
            return self.gen_struct_ctor(&name, args, ctx, out);
        }
        // FFI extern
        if let Some(ef) = self.externs.get(&name).cloned() {
            return self.gen_ffi(&name, &ef, args, ctx, out);
        }
        // user function (direct)
        if self.fns.contains_key(&name) {
            let sym = Self::sym(&name);
            let mut argvals = Vec::new();
            for a in args {
                argvals.push(self.gen_expr(a, ctx, out)?);
            }
            let sig = argvals
                .iter()
                .map(|v| format!("i64 {v}"))
                .collect::<Vec<_>>()
                .join(", ");
            // declare the user fn signature (all i64)
            let dsig = vec!["i64"; argvals.len()].join(", ");
            self.need(&sym, &format!("i64 @{sym}({dsig})"));
            let r = self.vreg();
            let _ = writeln!(out, "  {r} = call i64 @{sym}({sig})");
            return Ok(r);
        }
        Err(format!("undefined function '{name}'"))
    }

    fn gen_indirect(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<String, String> {
        if args.len() > 4 {
            return Err("indirect call supports up to 4 args".into());
        }
        let f = self.gen_expr(callee, ctx, out)?;
        let mut argvals = Vec::new();
        for a in args {
            argvals.push(self.gen_expr(a, ctx, out)?);
        }
        let n = argvals.len();
        let sym = format!("lumen_call{n}");
        self.need_call(&sym, n + 1);
        let mut parts = vec![format!("i64 {f}")];
        for v in &argvals {
            parts.push(format!("i64 {v}"));
        }
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = call i64 @{sym}({})", parts.join(", "));
        Ok(r)
    }

    fn gen_method(
        &mut self,
        obj: &Expr,
        name: &str,
        args: &[Expr],
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<String, String> {
        // module function call: math.sqrt(x), os.read(p), ...
        if let Expr::Ident(m) = obj {
            if crate::builtins::is_module(m) {
                let bf = crate::builtins::lookup(m, name)
                    .ok_or_else(|| format!("native {m}: no fn '{name}'"))?;
                let mut argvals = Vec::new();
                for a in args {
                    argvals.push(self.gen_expr(a, ctx, out)?);
                }
                self.need_call(bf.symbol, argvals.len());
                let sig = argvals
                    .iter()
                    .map(|v| format!("i64 {v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let r = self.vreg();
                let _ = writeln!(out, "  {r} = call i64 @{}({sig})", bf.symbol);
                return Ok(r);
            }
            // struct method on a typed receiver is handled generically below via
            // user method symbols when obj is a known struct instance. The MVP
            // resolves struct methods through field/lookup at runtime is not
            // available, so we support the explicit lm_Struct__method only when
            // the receiver type is statically a struct name (ctor result). For
            // dynamic receivers we fall through to builtin obj-methods.
        }

        // user-defined struct method: try every struct that defines `name`.
        // Receiver value is passed as self. We pick the method by name; if
        // multiple structs define it, this MVP uses the first - documented limit.
        let mut found: Option<String> = None;
        for ((sname, mname), _f) in &self.methods {
            if mname == name {
                found = Some(sname.clone());
                break;
            }
        }
        if let Some(sname) = found {
            let recv = self.gen_expr(obj, ctx, out)?;
            let mut argvals = vec![recv];
            for a in args {
                argvals.push(self.gen_expr(a, ctx, out)?);
            }
            let sym = Self::msym(&sname, name);
            let dsig = vec!["i64"; argvals.len()].join(", ");
            self.need(&sym, &format!("i64 @{sym}({dsig})"));
            let sig = argvals
                .iter()
                .map(|v| format!("i64 {v}"))
                .collect::<Vec<_>>()
                .join(", ");
            let r = self.vreg();
            let _ = writeln!(out, "  {r} = call i64 @{sym}({sig})");
            return Ok(r);
        }

        // `get`: 1-arg -> lumen_map_get(map,key); 2-arg -> lumen_map_get_or(map,key,dflt).
        if name == "get" {
            let recv = self.gen_expr(obj, ctx, out)?;
            let mut argvals = vec![recv];
            for a in args {
                argvals.push(self.gen_expr(a, ctx, out)?);
            }
            let sym = if args.len() == 2 {
                "lumen_map_get_or"
            } else {
                "lumen_map_get"
            };
            self.need_call(sym, argvals.len());
            let sig = argvals
                .iter()
                .map(|v| format!("i64 {v}"))
                .collect::<Vec<_>>()
                .join(", ");
            let r = self.vreg();
            let _ = writeln!(out, "  {r} = call i64 @{sym}({sig})");
            return Ok(r);
        }

        // built-in object methods (list/str/map): map to runtime helpers.
        // `len` is special: returns a raw int that must be boxed via from_int.
        if name == "len" && args.is_empty() {
            self.need("lumen_len", "i64 @lumen_len(i64)");
            self.need("lumen_from_int", "i64 @lumen_from_int(i64)");
            let recv = self.gen_expr(obj, ctx, out)?;
            let n = self.vreg();
            let _ = writeln!(out, "  {n} = call i64 @lumen_len(i64 {recv})");
            let r = self.vreg();
            let _ = writeln!(out, "  {r} = call i64 @lumen_from_int(i64 {n})");
            return Ok(r);
        }
        let recv = self.gen_expr(obj, ctx, out)?;
        if let Some((sym, n)) = builtin_method(name, args.len()) {
            // special: len/count return int -> already boxed by helper variants
            self.need_call(sym, n);
            let mut argvals = vec![recv];
            for a in args {
                argvals.push(self.gen_expr(a, ctx, out)?);
            }
            let sig = argvals
                .iter()
                .map(|v| format!("i64 {v}"))
                .collect::<Vec<_>>()
                .join(", ");
            let r = self.vreg();
            let _ = writeln!(out, "  {r} = call i64 @{sym}({sig})");
            return Ok(r);
        }
        Err(format!("unknown method '{name}'"))
    }

    // Named-argument call: only valid for struct construction (Point(x=3,y=4)).
    // Reorder the named args into the struct's field order, then build it via the
    // positional ctor. Mirrors the asm backend's gen_named_call + gen_struct_ctor.
    fn gen_named_call(
        &mut self,
        callee: &Expr,
        args: &[(String, Expr)],
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<String, String> {
        let name = match callee {
            Expr::Ident(n) => n.clone(),
            _ => return Err("a constructor call must name a struct type".into()),
        };
        let sdef = self
            .structs
            .get(&name)
            .cloned()
            .ok_or(format!("no struct type named '{name}'"))?;
        // map each field to its supplied value (by name), in field order
        let mut ordered: Vec<Expr> = Vec::with_capacity(sdef.fields.len());
        for fld in &sdef.fields {
            let val = args
                .iter()
                .find(|(n, _)| n == &fld.name)
                .map(|(_, e)| e.clone())
                .ok_or(format!("missing field '{}' for struct '{name}'", fld.name))?;
            ordered.push(val);
        }
        self.gen_struct_ctor(&name, &ordered, ctx, out)
    }

    fn gen_struct_ctor(
        &mut self,
        name: &str,
        args: &[Expr],
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<String, String> {
        let sdef = self.structs.get(name).cloned().ok_or("unknown struct")?;
        self.need("lumen_struct_new", "i64 @lumen_struct_new(ptr, i64, ptr)");
        self.need("lumen_struct_set", "void @lumen_struct_set(i64, ptr, i64)");
        let nfields = sdef.fields.len();
        // The field-names array MUST outlive this call: lumen_struct_new stores
        // the pointer without copying (rt.c). A stack alloca would dangle once we
        // return (crashes at -O0). So emit a module-level global array of name
        // constants, exactly like the asm backend's .rodata field tables.
        let names: Vec<String> = sdef.fields.iter().map(|f| f.name.clone()).collect();
        let arr = self.field_names_global(name, &names);
        let nm = self.add_str(name);
        let np = self.vreg();
        let _ = writeln!(out, "  {np} = {nm}");
        let sv = self.vreg();
        let _ = writeln!(
            out,
            "  {sv} = call i64 @lumen_struct_new(ptr {np}, i64 {nfields}, ptr {arr})"
        );
        let svslot = self.vreg();
        let _ = writeln!(out, "  {svslot} = alloca i64");
        let _ = writeln!(out, "  store i64 {sv}, ptr {svslot}");
        // set fields positionally
        for (i, fld) in sdef.fields.iter().enumerate() {
            if i < args.len() {
                let v = self.gen_expr(&args[i], ctx, out)?;
                let g = self.add_str(&fld.name);
                let fp = self.vreg();
                let _ = writeln!(out, "  {fp} = {g}");
                let s = self.vreg();
                let _ = writeln!(out, "  {s} = load i64, ptr {svslot}");
                let _ = writeln!(
                    out,
                    "  call void @lumen_struct_set(i64 {s}, ptr {fp}, i64 {v})"
                );
            }
        }
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = load i64, ptr {svslot}");
        Ok(r)
    }

    // Emit (once per struct) a module-level `[N x ptr]` global holding pointers
    // to each field-name C-string constant. Returns the global symbol, usable
    // directly as a `ptr` arg. Lives for the whole program, so lumen_struct_new
    // can keep the pointer.
    fn field_names_global(&mut self, sname: &str, names: &[String]) -> String {
        let gsym = format!("@.fields.{}", sanitize(sname));
        if self.declared.insert(gsym.clone()) {
            // each name needs a private string constant
            let mut elems = Vec::new();
            for nm in names {
                let (enc, n) = encode_cstr(nm);
                let sg = format!("@.fstr{}", self.str_count);
                self.str_count += 1;
                let _ = writeln!(
                    self.globals,
                    "{sg} = private unnamed_addr constant [{n} x i8] c\"{enc}\""
                );
                elems.push(format!("ptr {sg}"));
            }
            let n = names.len();
            let _ = writeln!(
                self.globals,
                "{gsym} = private unnamed_addr constant [{n} x ptr] [{}]",
                elems.join(", ")
            );
        }
        gsym
    }

    fn gen_closure(
        &mut self,
        fn_name: &str,
        captures: &[Expr],
        ctx: &mut Ctx,
        out: &mut String,
    ) -> Result<String, String> {
        let f = self
            .fns
            .get(fn_name)
            .cloned()
            .ok_or(format!("unknown closure '{fn_name}'"))?;
        let total = f.params.len();
        let ncap = captures.len();
        let user_arity = total - ncap;
        let sym = Self::sym(fn_name);
        // ensure the fn is declared (it's defined elsewhere in this module)
        let dsig = vec!["i64"; total].join(", ");
        self.need(&sym, &format!("i64 @{sym}({dsig})"));
        self.need("lumen_closure_new", "i64 @lumen_closure_new(ptr, i64, i64)");
        self.need(
            "lumen_closure_set_cap",
            "void @lumen_closure_set_cap(i64, i64, i64)",
        );
        let clo = self.vreg();
        let _ = writeln!(
            out,
            "  {clo} = call i64 @lumen_closure_new(ptr @{sym}, i64 {user_arity}, i64 {ncap})"
        );
        let cslot = self.vreg();
        let _ = writeln!(out, "  {cslot} = alloca i64");
        let _ = writeln!(out, "  store i64 {clo}, ptr {cslot}");
        for (i, cap) in captures.iter().enumerate() {
            let v = self.gen_expr(cap, ctx, out)?;
            let c = self.vreg();
            let _ = writeln!(out, "  {c} = load i64, ptr {cslot}");
            let _ = writeln!(
                out,
                "  call void @lumen_closure_set_cap(i64 {c}, i64 {i}, i64 {v})"
            );
        }
        let r = self.vreg();
        let _ = writeln!(out, "  {r} = load i64, ptr {cslot}");
        Ok(r)
    }

    fn gen_ffi(
        &mut self,
        _name: &str,
        _ef: &ExternFn,
        _args: &[Expr],
        _ctx: &mut Ctx,
        _out: &mut String,
    ) -> Result<String, String> {
        // FFI lowering is deferred (plan doc 04). For now produce a clear error
        // so `lumen build --backend llvm` fails loudly rather than miscompiling.
        Err("LLVM backend: extern/FFI not yet supported (use --backend asm)".into())
    }
}

// short helpers (module-level)

// Does this statement list contain a try anywhere (including nested)?
fn body_has_try(body: &[Stmt]) -> bool {
    body.iter().any(stmt_has_try)
}

fn stmt_has_try(s: &Stmt) -> bool {
    match s {
        Stmt::Try { .. } => true,
        Stmt::If {
            then, elifs, els, ..
        } => {
            body_has_try(then)
                || elifs.iter().any(|(_, b)| body_has_try(b))
                || els.as_ref().is_some_and(|b| body_has_try(b))
        }
        Stmt::While { body, .. } | Stmt::For { body, .. } => body_has_try(body),
        _ => false,
    }
}

fn runtime_op(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "lumen_add",
        BinOp::Sub => "lumen_sub",
        BinOp::Mul => "lumen_mul",
        BinOp::Pow => "lumen_pow",
        BinOp::Div => "lumen_div",
        BinOp::Mod => "lumen_mod",
        BinOp::Eq => "lumen_eq",
        BinOp::Ne => "lumen_ne",
        BinOp::Lt => "lumen_lt",
        BinOp::Le => "lumen_le",
        BinOp::Gt => "lumen_gt",
        BinOp::Ge => "lumen_ge",
        BinOp::In => "lumen_in",
        BinOp::NotIn => "lumen_not_in",
        BinOp::And | BinOp::Or => "unreachable",
    }
}

// list/str/map instance methods -> (runtime symbol, total arg count incl recv)
fn builtin_method(name: &str, nargs: usize) -> Option<(&'static str, usize)> {
    let sym = match name {
        "push" => "lumen_list_push",
        "pop" => "lumen_list_pop",
        "insert" => "lumen_list_insert",
        "reverse" => "lumen_list_reverse",
        "sort" => "lumen_list_sort",
        "index" => "lumen_list_index",
        "count" => "lumen_list_count",
        "contains" => "lumen_contains",
        "keys" => "lumen_map_keys",
        "values" => "lumen_map_values",
        "has" => "lumen_map_has",
        "get" => "lumen_map_get",
        "remove" => "lumen_map_remove",
        "upper" => "lumen_str_upper",
        "lower" => "lumen_str_lower",
        "strip" | "trim" => "lumen_str_trim",
        "lstrip" => "lumen_str_lstrip",
        "rstrip" => "lumen_str_rstrip",
        "split" => "lumen_str_split",
        "find" => "lumen_str_find",
        "replace" => "lumen_str_replace",
        "starts_with" => "lumen_str_starts_with",
        "ends_with" => "lumen_str_ends_with",
        "repeat" => "lumen_str_repeat",
        "title" => "lumen_str_title",
        "join" => "lumen_join",
        _ => return None,
    };
    Some((sym, nargs + 1))
}

// Encode a Rust string into an LLVM c"..." byte literal (with trailing NUL).
// Returns (encoded, total_byte_count_including_nul).
fn encode_cstr(s: &str) -> (String, usize) {
    let mut out = String::new();
    let mut n = 0usize;
    for b in s.bytes() {
        match b {
            b'\\' => {
                out.push_str("\\5C");
                n += 1;
            }
            b'"' => {
                out.push_str("\\22");
                n += 1;
            }
            0x20..=0x7E => {
                out.push(b as char);
                n += 1;
            }
            _ => {
                let _ = write!(out, "\\{b:02X}");
                n += 1;
            }
        }
    }
    out.push_str("\\00");
    n += 1;
    (out, n)
}

// sanitize a Lumen identifier for use inside an LLVM local name.
fn sanitize(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            s.push(c);
        } else {
            s.push('_');
        }
    }
    s
}

// True if e is an int literal that isn't zero. Used to allow the unboxed
// div/mod fast path only when the divisor can't trap (matches the runtime).
fn nonzero_lit(e: &Expr) -> bool {
    matches!(e, Expr::Int(n) if *n != 0)
}

fn is_cmp(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    )
}

// LLVM signed-int compare condition code for a Lumen compare op.
fn icmp_cc(op: BinOp) -> &'static str {
    match op {
        BinOp::Eq => "eq",
        BinOp::Ne => "ne",
        BinOp::Lt => "slt",
        BinOp::Le => "sle",
        BinOp::Gt => "sgt",
        BinOp::Ge => "sge",
        _ => unreachable!(),
    }
}

// LLVM ordered-float compare condition code for a Lumen compare op.
fn fcmp_cc(op: BinOp) -> &'static str {
    match op {
        BinOp::Eq => "oeq",
        BinOp::Ne => "one",
        BinOp::Lt => "olt",
        BinOp::Le => "ole",
        BinOp::Gt => "ogt",
        BinOp::Ge => "oge",
        _ => unreachable!(),
    }
}

// Print a double in LLVM hex form when it isn't exactly representable as a
// short decimal, so the bit pattern is preserved exactly (matches the asm
// backend's .quad bits approach -> byte-identical float behavior).
fn fmt_double(f: f64) -> String {
    format!("0x{:016X}", f.to_bits())
}
