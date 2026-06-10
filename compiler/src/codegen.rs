//! Native x86-64 backend (Intel syntax, Win64 ABI). Lowers the AST to assembly
//! that must match interp.rs byte-for-byte. Values are NaN-boxed 64-bit words,
//! but proven-int and proven-float locals get raw fast paths that skip the box.
//! Backed by the C runtime in runtime/lumen_rt.c. The GC scans the native stack
//! conservatively, so anything we want kept alive across a call must live in a
//! frame slot, not just a register, at every alloc/poll point.
//! 
//! DO NOT TOUCH UNLESS YOU UNDRSTAND THIS FILE, THIS IS VERY IMPORTANT!

use crate::ast::*;
use crate::types::IntInfo;
use std::collections::HashMap;
use std::fmt::Write;

pub struct Codegen {
    rodata: String,
    str_count: usize,
    label_count: usize,
    externs: HashMap<String, ExternFn>,
    fns: HashMap<String, FnDef>,
    structs: HashMap<String, StructDef>,
    methods: HashMap<(String, String), FnDef>,
    field_tables: String,
    int_info: IntInfo,

    raw_entry_fns: std::collections::HashSet<String>,

    mir_sigs: crate::mir::SigMap,

    cur_line: u32,
}

struct FnCtx {
    locals: HashMap<String, i32>,
    stack_size: i32,
    loop_stack: Vec<(String, String)>,
    struct_hint: Option<String>,
    func_name: String,

    raw_int: std::collections::HashSet<String>,

    raw_float: std::collections::HashSet<String>,

    raw_return: bool,

    tail_label: Option<String>,
    tail_params: Vec<(i32, bool)>,

    callee_saves: Vec<(&'static str, i32)>,
    next_callee: usize,

    float_loc: std::collections::HashMap<String, &'static str>,
    float_xmm_saves: Vec<(&'static str, i32)>,
    next_xmm: usize,

    int_loc: std::collections::HashMap<String, String>,
}

// Caller-clobbered we cannot keep, so these are the callee-saved regs/xmm we
// borrow to keep hot loop accumulators out of memory. We save/restore them in
// the prologue/epilogue (see gen_fn_impl).
const XMM_POOL: [&str; 4] = ["xmm6", "xmm7", "xmm8", "xmm9"];

const CALLEE_POOL: [&str; 4] = ["r12", "r13", "r14", "r15"];

impl FnCtx {

    fn is_raw(&self, name: &str) -> bool {
        self.raw_int.contains(name)
    }

    fn is_raw_float(&self, name: &str) -> bool {
        self.raw_float.contains(name)
    }
}

impl Default for Codegen {
    fn default() -> Self {
        Self::new()
    }
}

impl Codegen {
    pub fn new() -> Self {
        Codegen {
            rodata: String::new(),
            str_count: 0,
            label_count: 0,
            externs: HashMap::new(),
            fns: HashMap::new(),
            structs: HashMap::new(),
            methods: HashMap::new(),
            field_tables: String::new(),
            int_info: IntInfo::default(),
            raw_entry_fns: std::collections::HashSet::new(),
            mir_sigs: crate::mir::SigMap::default(),
            cur_line: 0,
        }
    }

    fn err(&self, msg: impl std::fmt::Display) -> String {
        if self.cur_line > 0 {
            format!("{msg} (line {})", self.cur_line)
        } else {
            msg.to_string()
        }
    }

    // Cooperative GC safepoint: if the runtime has allocated enough since the
    // last collection, call into the collector here. We only poll at points
    // where the stack is in a scannable state (live values spilled to slots).
    fn emit_gc_poll(&mut self, out: &mut String) {
        let skip = self.new_label("gcskip");
        out.push_str("    cmp qword ptr [rip + lumen_allocs_since_gc], 512\n");
        let _ = writeln!(out, "    jl {skip}");
        out.push_str("    call lumen_gc_collect\n");
        let _ = writeln!(out, "{skip}:");
    }

    fn emit_gc_poll_cond(&mut self, body: &[Stmt], out: &mut String) {
        if !Self::body_no_alloc(body) {
            self.emit_gc_poll(out);
        }
    }

    fn new_label(&mut self, base: &str) -> String {
        self.label_count += 1;
        format!(".L{}_{}", base, self.label_count)
    }

    fn new_unique(&mut self) -> usize {
        self.label_count += 1;
        self.label_count
    }

    // Ints live in the low 48 bits of a NaN-boxed word. Sign-extend them back to
    // a full 64-bit value by shifting up 16 and arithmetic-shifting down.
    fn emit_unbox_int(out: &mut String) {

        out.push_str("    shl rax, 16\n    sar rax, 16\n");
    }

    // Re-box a raw 64-bit int: keep the low 48 bits, then OR in the int tag in
    // the high 16. The mask drops any garbage in bits 48-63 first.
    fn emit_box_int(out: &mut String) {
        out.push_str("    mov rcx, 0xFFFFFFFFFFFF\n    and rax, rcx\n");
        out.push_str("    mov rcx, 0x7FF9000000000000\n    or rax, rcx\n");
    }

    fn add_string(&mut self, s: &str) -> String {
        let label = format!(".str{}", self.str_count);
        self.str_count += 1;
        let _ = writeln!(self.rodata, "{}: .asciz \"{}\"", label, escape(s));
        label
    }

    fn add_double(&mut self, bits: u64) -> String {
        let label = format!(".fconst{}", self.str_count);
        self.str_count += 1;
        let _ = writeln!(self.rodata, "{label}: .quad {bits}");
        label
    }

    fn fnsym(name: &str) -> String {
        format!("lm_{name}")
    }

    fn fnsym_raw(name: &str) -> String {
        format!("lm_{name}.raw")
    }
    fn methodsym(struct_name: &str, method: &str) -> String {
        format!("lm_{struct_name}__{method}")
    }

    pub fn generate(&mut self, prog: &Program) -> Result<String, String> {
        self.int_info = crate::types::analyze(prog);

        self.mir_sigs = crate::mir::SigMap::from_program(prog);

        let mut top_stmts: Vec<Stmt> = Vec::new();
        for item in prog {
            match item {
                Item::Fn(f) => {
                    self.fns.insert(f.name.clone(), f.clone());
                }
                Item::ExternBlock(b) => {
                    for ef in &b.fns {
                        self.externs.insert(ef.name.clone(), ef.clone());
                    }
                }
                Item::Struct(s) => {
                    let entry = self
                        .structs
                        .entry(s.name.clone())
                        .or_insert_with(|| StructDef {
                            name: s.name.clone(),
                            fields: Vec::new(),
                            methods: Vec::new(),
                        });
                    if !s.fields.is_empty() {
                        entry.fields = s.fields.clone();
                    }
                    for m in &s.methods {
                        self.methods
                            .insert((s.name.clone(), m.name.clone()), m.clone());
                    }
                }
                Item::Stmt(s) => top_stmts.push(s.clone()),
                _ => {}
            }
        }

        let main_called = top_stmts.iter().any(|s| {
            matches!(s,
                Stmt::ExprStmt(Expr::Call { callee, .. })
                    if matches!(&**callee, Expr::Ident(n) if n == "main"))
        });
        let mut entry_body = top_stmts;
        if !main_called && self.fns.contains_key("main") {
            entry_body.push(Stmt::ExprStmt(Expr::Call {
                callee: Box::new(Expr::Ident("main".to_string())),
                args: Vec::new(),
            }));
        }
        let entry_fn = FnDef {
            name: "__lumen_entry__".to_string(),
            params: Vec::new(),
            ret: Type::Unknown,
            body: entry_body,
            exported: false,
            is_method: false,
        };

        let mut out = String::new();
        out.push_str("    .intel_syntax noprefix\n    .text\n    .globl main\n\n");

        // Emit in source order (deterministic). Iterating self.fns (a HashMap)
        // would randomize fn order per build, breaking reproducible output.
        let fns: Vec<FnDef> = prog
            .iter()
            .filter_map(|it| match it {
                Item::Fn(f) => Some(f.clone()),
                _ => None,
            })
            .collect();

        // A function gets a second ".raw" entry point (unboxed Win64 ABI: ints in
        // registers, no NaN-box) when both its params and return are proven int.
        // Raw callers jump straight to it and skip all the box/unbox churn.
        self.raw_entry_fns = fns
            .iter()
            .filter(|f| {
                !f.is_method
                    && !f.params.is_empty()
                    && self.int_info.int_ret.contains(&f.name)
                    && f.params
                        .iter()
                        .all(|p| self.int_info.is_int_var(&f.name, &p.name))
            })
            .map(|f| f.name.clone())
            .collect();
        for f in &fns {
            let body = self.gen_fn(&Self::fnsym(&f.name), f, None)?;
            out.push_str(&body);
            out.push('\n');

            if self.raw_entry_fns.contains(&f.name) {
                let raw = self.gen_fn_raw(&Self::fnsym_raw(&f.name), f)?;
                out.push_str(&raw);
                out.push('\n');
            }
        }

        let entry_asm = self.gen_fn("lumen_user_main", &entry_fn, None)?;
        out.push_str(&entry_asm);
        out.push('\n');
        let methods: Vec<((String, String), FnDef)> = self
            .methods
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for ((sname, _mname), f) in &methods {
            let sym = Self::methodsym(sname, &f.name);
            let body = self.gen_fn(&sym, f, Some(sname.clone()))?;
            out.push_str(&body);
            out.push('\n');
        }

        out.push_str("main:\n    push rbp\n    mov rbp, rsp\n    sub rsp, 32\n");

        out.push_str("    call lumen_set_args\n");

        // Hand the GC the base of the native stack (rbp here) so its conservative
        // scan knows where to start walking for live, possibly-pointer words.
        out.push_str("    mov rcx, rbp\n    call lumen_gc_init\n");
        out.push_str("    call lumen_user_main\n    xor eax, eax\n");
        out.push_str("    add rsp, 32\n    pop rbp\n    ret\n\n");

        out.push_str("\n    .section .rodata\n");
        out.push_str(&self.field_tables);
        out.push_str(&self.rodata);
        Ok(peephole(&out))
    }

    fn gen_fn(
        &mut self,
        sym: &str,
        f: &FnDef,
        struct_hint: Option<String>,
    ) -> Result<String, String> {
        self.gen_fn_impl(sym, f, struct_hint, false)
    }

    fn ident_used_in_body(name: &str, body: &[Stmt]) -> bool {
        body.iter().any(|s| Self::ident_used_in_stmt(name, s))
    }

    fn assigned_in_body(name: &str, body: &[Stmt]) -> bool {
        body.iter().any(|s| Self::assigned_in_stmt(name, s))
    }

    fn assigned_in_stmt(name: &str, s: &Stmt) -> bool {
        match s {
            Stmt::Assign { target, .. } => matches!(target, Expr::Ident(n) if n == name),
            Stmt::Let { name: n, .. } => n == name,
            Stmt::If {
                then, elifs, els, ..
            } => {
                then.iter().any(|s| Self::assigned_in_stmt(name, s))
                    || elifs
                        .iter()
                        .any(|(_, b)| b.iter().any(|s| Self::assigned_in_stmt(name, s)))
                    || els
                        .as_ref()
                        .is_some_and(|b| b.iter().any(|s| Self::assigned_in_stmt(name, s)))
            }
            Stmt::While { body, .. } => body.iter().any(|s| Self::assigned_in_stmt(name, s)),

            Stmt::For { var, body, .. } => {
                var == name || body.iter().any(|s| Self::assigned_in_stmt(name, s))
            }
            _ => false,
        }
    }

    fn ident_used_in_stmt(name: &str, s: &Stmt) -> bool {
        match s {
            Stmt::Let { value, .. } => Self::ident_used_in_expr(name, value),
            Stmt::Assign { target, value } => {
                Self::ident_used_in_expr(name, target) || Self::ident_used_in_expr(name, value)
            }
            Stmt::ExprStmt(e) => Self::ident_used_in_expr(name, e),
            Stmt::Return(opt) => opt
                .as_ref()
                .is_some_and(|e| Self::ident_used_in_expr(name, e)),
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } => {
                Self::ident_used_in_expr(name, cond)
                    || then.iter().any(|s| Self::ident_used_in_stmt(name, s))
                    || elifs.iter().any(|(c, b)| {
                        Self::ident_used_in_expr(name, c)
                            || b.iter().any(|s| Self::ident_used_in_stmt(name, s))
                    })
                    || els
                        .as_ref()
                        .is_some_and(|b| b.iter().any(|s| Self::ident_used_in_stmt(name, s)))
            }
            Stmt::While { cond, body } => {
                Self::ident_used_in_expr(name, cond)
                    || body.iter().any(|s| Self::ident_used_in_stmt(name, s))
            }
            Stmt::For { iter, body, .. } => {

                Self::ident_used_in_expr(name, iter)
                    || body.iter().any(|s| Self::ident_used_in_stmt(name, s))
            }
            Stmt::Try {
                body, catch_body, ..
            } => {
                body.iter().any(|s| Self::ident_used_in_stmt(name, s))
                    || catch_body.iter().any(|s| Self::ident_used_in_stmt(name, s))
            }
            Stmt::Raise(e) => Self::ident_used_in_expr(name, e),
            Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => false,
        }
    }

    fn ident_used_in_expr(name: &str, e: &Expr) -> bool {
        match e {
            Expr::Ident(n) => n == name,
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Nil
            | Expr::SelfExpr => false,
            Expr::Unary { expr, .. } => Self::ident_used_in_expr(name, expr),
            Expr::Binary { lhs, rhs, .. } => {
                Self::ident_used_in_expr(name, lhs) || Self::ident_used_in_expr(name, rhs)
            }
            Expr::Call { callee, args } => {
                Self::ident_used_in_expr(name, callee)
                    || args.iter().any(|a| Self::ident_used_in_expr(name, a))
            }
            Expr::NamedCall { callee, args } => {
                Self::ident_used_in_expr(name, callee)
                    || args.iter().any(|(_, a)| Self::ident_used_in_expr(name, a))
            }
            Expr::Field { obj, .. } => Self::ident_used_in_expr(name, obj),
            Expr::Method { obj, args, .. } => {
                Self::ident_used_in_expr(name, obj)
                    || args.iter().any(|a| Self::ident_used_in_expr(name, a))
            }
            Expr::List(xs) => xs.iter().any(|x| Self::ident_used_in_expr(name, x)),
            Expr::Map(kvs) => kvs.iter().any(|(k, v)| {
                Self::ident_used_in_expr(name, k) || Self::ident_used_in_expr(name, v)
            }),
            Expr::Range { lo, hi } => {
                Self::ident_used_in_expr(name, lo) || Self::ident_used_in_expr(name, hi)
            }
            Expr::IfElse { cond, then, els } => {
                Self::ident_used_in_expr(name, cond)
                    || Self::ident_used_in_expr(name, then)
                    || Self::ident_used_in_expr(name, els)
            }
            Expr::ListComp {
                elem, iter, cond, ..
            } => {
                Self::ident_used_in_expr(name, elem)
                    || Self::ident_used_in_expr(name, iter)
                    || cond
                        .as_ref()
                        .is_some_and(|c| Self::ident_used_in_expr(name, c))
            }
            Expr::Index { obj, index } => {
                Self::ident_used_in_expr(name, obj) || Self::ident_used_in_expr(name, index)
            }
            Expr::Slice { obj, lo, hi } => {
                Self::ident_used_in_expr(name, obj)
                    || lo
                        .as_ref()
                        .is_some_and(|e| Self::ident_used_in_expr(name, e))
                    || hi
                        .as_ref()
                        .is_some_and(|e| Self::ident_used_in_expr(name, e))
            }
            Expr::FStr(parts) => parts.iter().any(|p| match p {
                FStrPart::Expr(pe) => Self::ident_used_in_expr(name, pe),
                _ => false,
            }),

            Expr::Lambda { body, .. } => body.iter().any(|s| Self::ident_used_in_stmt(name, s)),
            Expr::Closure { captures, .. } => {
                captures.iter().any(|c| Self::ident_used_in_expr(name, c))
            }
        }
    }

    fn float_accum_safe(var: &str, body: &[Stmt]) -> bool {
        body.iter().all(|s| match s {

            Stmt::Assign {
                target: Expr::Ident(t),
                value,
            } if t == var => Self::float_rhs_safe(var, value),

            _ => !Self::ident_used_in_stmt(var, s),
        })
    }

    // An int var is a safe register accumulator across a loop body when every use
    // is either (a) an assignment `var = <expr>` whose rhs reads var only as a
    // plain value (no escape), or (b) a read in a non-assigning statement that
    // doesn't let it escape into an alias. Mirrors float_accum_safe. The var must
    // also be a known raw-int so its slot holds an unboxed i64.
    fn int_accum_safe(var: &str, body: &[Stmt]) -> bool {
        body.iter().all(|s| match s {
            Stmt::Assign {
                target: Expr::Ident(t),
                value,
            } if t == var => Self::int_rhs_safe(var, value),
            _ => !Self::ident_used_in_stmt(var, s),
        })
    }

    fn int_rhs_safe(var: &str, e: &Expr) -> bool {
        match e {
            Expr::Ident(_) => true,
            Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Nil => true,
            Expr::Unary { expr, .. } => Self::int_rhs_safe(var, expr),
            Expr::Binary { lhs, rhs, .. } => {
                Self::int_rhs_safe(var, lhs) && Self::int_rhs_safe(var, rhs)
            }
            Expr::Index { obj, index } => {
                // Reading an element is fine; the var must not appear inside the
                // receiver/index in a way that aliases it (it can be the index value).
                Self::int_rhs_safe(var, obj) && Self::int_rhs_safe(var, index)
            }
            other => !Self::ident_used_in_expr(var, other),
        }
    }

    fn float_rhs_safe(var: &str, e: &Expr) -> bool {
        match e {

            Expr::Ident(n) => {
                let _ = n;
                true
            }
            Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Nil => true,

            Expr::Unary { expr, .. } => Self::float_rhs_safe(var, expr),
            Expr::Binary { lhs, rhs, .. } => {
                Self::float_rhs_safe(var, lhs) && Self::float_rhs_safe(var, rhs)
            }

            other => !Self::ident_used_in_expr(var, other),
        }
    }

    fn acquire_xmm_reg(&self, ctx: &mut FnCtx) -> Option<&'static str> {
        if ctx.next_xmm >= XMM_POOL.len() {
            return None;
        }
        let reg = XMM_POOL[ctx.next_xmm];
        ctx.next_xmm += 1;

        ctx.stack_size += 16;
        let slot = -ctx.stack_size;
        ctx.float_xmm_saves.push((reg, slot));
        Some(reg)
    }

    fn has_self_tail_call(body: &[Stmt], fname: &str) -> bool {
        body.iter().any(|s| Self::stmt_has_tail_call(s, fname))
    }

    // Peels a leading `if cond { return x }` guard off a raw function and emits it
    // as a pre-prologue branch straight into registers (no frame, no spill). This
    // is the recursion base-case fast path, e.g. fib(n) returning early for n < 2.
    // Returns the asm plus how many body statements were consumed.
    fn raw_base_fast(&mut self, f: &FnDef, raw_abi: bool) -> Option<(String, usize)> {
        if !raw_abi {
            return None;
        }

        let mut idx = 0;
        if matches!(f.body.first(), Some(Stmt::SrcLine(_))) {
            idx = 1;
        }
        let if_stmt = f.body.get(idx)?;
        let (cond, then) = match if_stmt {
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } if elifs.is_empty() && els.is_none() => (cond, then),
            _ => return None,
        };

        let ret_expr = match then.as_slice() {
            [Stmt::Return(Some(e))] => e,
            [Stmt::SrcLine(_), Stmt::Return(Some(e))] => e,
            _ => return None,
        };

        let arg_regs = ["rcx", "rdx", "r8", "r9"];
        let param_reg = |name: &str| -> Option<&'static str> {
            f.params
                .iter()
                .position(|p| p.name == name)
                .filter(|&i| i < 4)
                .map(|i| arg_regs[i])
        };

        let ret_to_rax = |out: &mut String| -> bool {
            match ret_expr {
                Expr::Ident(n) => match param_reg(n) {
                    Some(r) => {
                        let _ = writeln!(out, "    mov rax, {r}");
                        true
                    }
                    None => false,
                },
                Expr::Int(v) => {
                    let _ = writeln!(out, "    mov rax, {v}");
                    true
                }
                _ => false,
            }
        };

        let (op, lhs, rhs) = match cond {
            Expr::Binary { op, lhs, rhs } if Self::is_cmp_op(*op) => (*op, &**lhs, &**rhs),
            _ => return None,
        };

        let lhs_reg = match lhs {
            Expr::Ident(n) => param_reg(n)?,
            _ => return None,
        };

        let rhs_str = match rhs {
            Expr::Int(v) if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 => format!("{v}"),
            Expr::Ident(n) => param_reg(n)?.to_string(),
            _ => return None,
        };

        let skip = self.new_label("noskip");
        let jcc_false = Self::jcc_for_cmp_false(op);
        let mut fast = String::new();
        let _ = writeln!(fast, "    cmp {lhs_reg}, {rhs_str}");
        let _ = writeln!(fast, "    {jcc_false} {skip}");
        if !ret_to_rax(&mut fast) {
            return None;
        }
        fast.push_str("    ret\n");
        let _ = writeln!(fast, "{skip}:");
        Some((fast, idx + 1))
    }

    fn is_cmp_op(op: BinOp) -> bool {
        matches!(
            op,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne
        )
    }

    fn jcc_for_cmp_false(op: BinOp) -> &'static str {
        match op {
            BinOp::Lt => "jge",
            BinOp::Le => "jg",
            BinOp::Gt => "jle",
            BinOp::Ge => "jl",
            BinOp::Eq => "jne",
            BinOp::Ne => "je",
            _ => unreachable!("jcc_for_cmp_false on non-comparison"),
        }
    }

    fn body_no_alloc(body: &[Stmt]) -> bool {
        body.iter().all(Self::stmt_no_alloc)
    }

    fn stmt_no_alloc(s: &Stmt) -> bool {
        match s {
            Stmt::Let { value, .. } => Self::expr_no_alloc(value),
            Stmt::Assign { target, value } => {
                Self::expr_no_alloc(target) && Self::expr_no_alloc(value)
            }
            Stmt::ExprStmt(e) => Self::expr_no_alloc(e),
            Stmt::Return(opt) => opt.as_ref().map(Self::expr_no_alloc).unwrap_or(true),
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } => {
                Self::expr_no_alloc(cond)
                    && then.iter().all(Self::stmt_no_alloc)
                    && elifs
                        .iter()
                        .all(|(c, b)| Self::expr_no_alloc(c) && b.iter().all(Self::stmt_no_alloc))
                    && els
                        .as_ref()
                        .map(|b| b.iter().all(Self::stmt_no_alloc))
                        .unwrap_or(true)
            }
            Stmt::While { cond, body } => {
                Self::expr_no_alloc(cond) && body.iter().all(Self::stmt_no_alloc)
            }
            Stmt::For { iter, body, .. } => {
                Self::expr_no_alloc(iter) && body.iter().all(Self::stmt_no_alloc)
            }
            Stmt::Try {
                body, catch_body, ..
            } => body.iter().all(Self::stmt_no_alloc) && catch_body.iter().all(Self::stmt_no_alloc),

            Stmt::Raise(_) => false,
            Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => true,
        }
    }

    fn expr_no_alloc(e: &Expr) -> bool {
        match e {
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Nil | Expr::Ident(_) => true,
            Expr::Unary { expr, .. } => Self::expr_no_alloc(expr),
            Expr::Binary { lhs, rhs, .. } => Self::expr_no_alloc(lhs) && Self::expr_no_alloc(rhs),
            Expr::Range { lo, hi } => Self::expr_no_alloc(lo) && Self::expr_no_alloc(hi),
            Expr::IfElse { cond, then, els } => {
                Self::expr_no_alloc(cond) && Self::expr_no_alloc(then) && Self::expr_no_alloc(els)
            }

            _ => false,
        }
    }

    fn stmt_has_tail_call(s: &Stmt, fname: &str) -> bool {
        match s {
            Stmt::Return(Some(Expr::Call { callee, .. })) => {
                matches!(&**callee, Expr::Ident(n) if n == fname)
            }
            Stmt::If {
                then, elifs, els, ..
            } => {
                then.iter().any(|s| Self::stmt_has_tail_call(s, fname))
                    || elifs
                        .iter()
                        .any(|(_, b)| b.iter().any(|s| Self::stmt_has_tail_call(s, fname)))
                    || els
                        .as_ref()
                        .is_some_and(|b| b.iter().any(|s| Self::stmt_has_tail_call(s, fname)))
            }
            _ => false,
        }
    }

    fn gen_fn_raw(&mut self, sym: &str, f: &FnDef) -> Result<String, String> {
        self.gen_fn_impl(sym, f, None, true)
    }

    fn gen_fn_impl(
        &mut self,
        sym: &str,
        f: &FnDef,
        struct_hint: Option<String>,
        raw_abi: bool,
    ) -> Result<String, String> {

        // MIR numeric tier (SSA + linear-scan regalloc) is opt-in via LUMEN_MIR=1
        // while it's proven byte-identical against the AST path (see plan S2).
        let mir_on = std::env::var("LUMEN_MIR").as_deref() == Ok("1");
        let mut mir_body: Option<String> = None;
        if mir_on && raw_abi && crate::mir::mir_eligible(f, &self.int_info) {
            if let Ok(mir) = crate::mir::lower_fn(f, &self.mir_sigs) {
                if Self::mir_all_int(&mir) {
                    let ra = crate::mir::regalloc(&mir);

                    let mut mctx = FnCtx {
                        locals: HashMap::new(),
                        stack_size: 0,
                        loop_stack: Vec::new(),
                        struct_hint: None,
                        func_name: f.name.clone(),
                        raw_int: self
                            .int_info
                            .int_vars
                            .get(&f.name)
                            .cloned()
                            .unwrap_or_default(),
                        raw_float: Default::default(),
                        raw_return: true,
                        tail_label: None,
                        tail_params: Vec::new(),
                        callee_saves: Vec::new(),
                        next_callee: 0,
                        float_loc: std::collections::HashMap::new(),
                        float_xmm_saves: Vec::new(),
                        next_xmm: 0,
                        int_loc: std::collections::HashMap::new(),
                    };

                    let arg_regs = ["rcx", "rdx", "r8", "r9"];
                    let mut mprologue = String::new();
                    for (i, p) in f.params.iter().enumerate() {
                        mctx.stack_size += 8;
                        let off = -mctx.stack_size;
                        mctx.locals.insert(p.name.clone(), off);
                        if i < 4 {
                            let _ = writeln!(mprologue, "    mov [rbp{}], {}", off, arg_regs[i]);
                        } else {
                            let caller_off = 48 + (i as i32 - 4) * 8;
                            let _ = writeln!(mprologue, "    mov rax, [rbp+{caller_off}]");
                            let _ = writeln!(mprologue, "    mov [rbp{off}], rax");
                        }
                    }
                    if let Ok(body) = self.gen_fn_mir_body(&mir, &ra, f, &mut mctx) {

                        let mut save = String::new();
                        let mut restore = String::new();
                        for reg in &ra.callee_saved_used {
                            mctx.stack_size += 8;
                            let slot = -mctx.stack_size;
                            let _ = writeln!(save, "    mov [rbp{slot}], {reg}");
                            let _ = writeln!(restore, "    mov {reg}, [rbp{slot}]");
                        }
                        let frame = ((mctx.stack_size + 32 + 15) / 16) * 16;
                        let mut out = String::new();
                        let _ = writeln!(out, "{sym}:");
                        out.push_str("    push rbp\n    mov rbp, rsp\n");
                        let _ = writeln!(out, "    sub rsp, {frame}");
                        out.push_str(&save);
                        out.push_str(&mprologue);
                        out.push_str(&body);

                        out.push_str("    xor eax, eax\n    mov rsp, rbp\n    pop rbp\n    ret\n");

                        if !restore.is_empty() {
                            let ret_seq = "    mov rsp, rbp\n    pop rbp\n    ret\n";
                            let replacement = format!("{restore}{ret_seq}");
                            out = out.replace(ret_seq, &replacement);
                        }
                        mir_body = Some(out);
                    }
                }
            }
        }
        if let Some(asm) = mir_body {
            return Ok(asm);
        }
        let mut ctx = FnCtx {
            locals: HashMap::new(),
            stack_size: 0,
            loop_stack: Vec::new(),
            struct_hint,
            func_name: f.name.clone(),

            raw_int: self
                .int_info
                .int_vars
                .get(&f.name)
                .cloned()
                .unwrap_or_default(),

            raw_float: self
                .int_info
                .float_vars
                .get(&f.name)
                .cloned()
                .unwrap_or_default(),

            raw_return: raw_abi,

            tail_label: None,
            tail_params: Vec::new(),
            callee_saves: Vec::new(),
            next_callee: 0,
            float_loc: std::collections::HashMap::new(),
            float_xmm_saves: Vec::new(),
            next_xmm: 0,
            int_loc: std::collections::HashMap::new(),
        };
        let arg_regs = ["rcx", "rdx", "r8", "r9"];
        let mut prologue = String::new();
        for (i, p) in f.params.iter().enumerate() {
            ctx.stack_size += 8;
            let off = -ctx.stack_size;
            ctx.locals.insert(p.name.clone(), off);

            let raw = ctx.raw_int.contains(&p.name);
            if i < 4 {
                // Boxed entry receiving a raw-int param: unbox the incoming NaN-boxed
                // arg into a plain int once, here, so the body can use it raw. The
                // .raw entry (raw_abi) already gets plain ints, so it skips this.
                if raw && !raw_abi {
                    let _ = writeln!(prologue, "    mov rax, {}", arg_regs[i]);
                    prologue.push_str("    shl rax, 16\n    sar rax, 16\n");
                    let _ = writeln!(prologue, "    mov [rbp{off}], rax");
                } else {
                    let _ = writeln!(prologue, "    mov [rbp{}], {}", off, arg_regs[i]);
                }
            } else {

                // Args 5+ are passed on the caller's stack. After our push rbp
                // (8) + return addr (8) + the 32-byte Win64 shadow space, the
                // first stack arg sits at rbp+48.
                let caller_off = 48 + (i as i32 - 4) * 8;
                let _ = writeln!(prologue, "    mov rax, [rbp+{caller_off}]");
                if raw && !raw_abi {
                    prologue.push_str("    shl rax, 16\n    sar rax, 16\n");
                }
                let _ = writeln!(prologue, "    mov [rbp{off}], rax");
            }
        }

        // Self-recursive tail call: instead of recursing, we'll overwrite the
        // param slots and jump back to a label just after the prologue. Only
        // for plain functions (no struct_hint/method) so the frame layout is
        // stable across iterations. Records each param's slot and rawness.
        if ctx.struct_hint.is_none() && !f.is_method && Self::has_self_tail_call(&f.body, &f.name) {
            let label = self.new_label("tailtop");
            let tparams: Vec<(i32, bool)> = f
                .params
                .iter()
                .map(|p| (*ctx.locals.get(&p.name).unwrap(), ctx.is_raw(&p.name)))
                .collect();
            ctx.tail_label = Some(label);
            ctx.tail_params = tparams;
        }

        let mut body = String::new();

        if let Some(lbl) = &ctx.tail_label {
            let _ = writeln!(body, "{lbl}:");
        }

        let mut fastpath = String::new();
        let body_start = if ctx.tail_label.is_none() {
            match self.raw_base_fast(f, raw_abi) {
                Some((fast, start)) => {
                    fastpath = fast;
                    start
                }
                None => 0,
            }
        } else {
            0
        };
        for s in &f.body[body_start..] {
            self.gen_stmt(s, &mut ctx, &mut body)?;
        }

        // Round the frame up to a 16-byte multiple. The +32 reserves Win64
        // shadow space for any callee; keeping rsp 16-aligned at call sites is
        // an ABI requirement (movaps and friends fault otherwise).
        let frame = ((ctx.stack_size + 32 + 15) / 16) * 16;
        let mut out = String::new();
        let _ = writeln!(out, "{sym}:");
        out.push_str(&fastpath);
        out.push_str("    push rbp\n    mov rbp, rsp\n");
        let _ = writeln!(out, "    sub rsp, {frame}");

        for (reg, slot) in &ctx.callee_saves {
            let _ = writeln!(out, "    mov [rbp{slot}], {reg}");
        }

        for (reg, slot) in &ctx.float_xmm_saves {
            let _ = writeln!(out, "    movdqu [rbp{slot}], {reg}");
        }
        out.push_str(&prologue);
        out.push_str(&body);

        if raw_abi {
            out.push_str("    xor eax, eax\n    mov rsp, rbp\n    pop rbp\n    ret\n");
        } else {
            out.push_str("    call lumen_nil\n    mov rsp, rbp\n    pop rbp\n    ret\n");
        }

        if !ctx.callee_saves.is_empty() || !ctx.float_xmm_saves.is_empty() {
            let mut restore = String::new();
            for (reg, slot) in ctx.callee_saves.iter().rev() {
                let _ = writeln!(restore, "    mov {reg}, [rbp{slot}]");
            }

            for (reg, slot) in ctx.float_xmm_saves.iter().rev() {
                let _ = writeln!(restore, "    movdqu {reg}, [rbp{slot}]");
            }
            let ret_seq = "    mov rsp, rbp\n    pop rbp\n    ret\n";
            let replacement = format!("{restore}{ret_seq}");
            out = out.replace(ret_seq, &replacement);
        }
        Ok(out)
    }

    fn slot(&self, ctx: &mut FnCtx, name: &str) -> i32 {
        if let Some(&o) = ctx.locals.get(name) {
            return o;
        }
        ctx.stack_size += 8;
        let off = -ctx.stack_size;
        ctx.locals.insert(name.to_string(), off);
        off
    }
    fn temp(&self, ctx: &mut FnCtx) -> i32 {
        ctx.stack_size += 8;
        -ctx.stack_size
    }

    fn acquire_callee_reg(&self, ctx: &mut FnCtx) -> Option<&'static str> {
        if ctx.next_callee >= CALLEE_POOL.len() {
            return None;
        }
        let reg = CALLEE_POOL[ctx.next_callee];
        ctx.next_callee += 1;
        ctx.stack_size += 8;
        let slot = -ctx.stack_size;
        ctx.callee_saves.push((reg, slot));
        Some(reg)
    }

    fn gen_stmt(&mut self, s: &Stmt, ctx: &mut FnCtx, out: &mut String) -> Result<(), String> {
        match s {
            Stmt::Let { name, value, .. } => {
                let off = self.slot(ctx, name);
                if ctx.is_raw(name) {

                    self.eval_raw(value, ctx, out)?;
                    let _ = writeln!(out, "    mov [rbp{off}], rax");
                } else if ctx.is_raw_float(name) {

                    self.eval_raw_float(value, ctx, out)?;

                    if let Some(reg) = ctx.float_loc.get(name) {
                        let _ = writeln!(out, "    movapd {reg}, xmm0");
                    } else {
                        let _ = writeln!(out, "    movsd [rbp{off}], xmm0");
                    }
                } else {
                    self.gen_expr(value, ctx, out)?;
                    let _ = writeln!(out, "    mov [rbp{off}], rax");
                }
            }
            Stmt::Assign { target, value } => match target {
                Expr::Ident(n) => {
                    let off = self.slot(ctx, n);
                    if ctx.is_raw(n) {
                        self.eval_raw(value, ctx, out)?;
                        if let Some(reg) = ctx.int_loc.get(n) {
                            let _ = writeln!(out, "    mov {reg}, rax");
                        } else {
                            let _ = writeln!(out, "    mov [rbp{off}], rax");
                        }
                    } else if ctx.is_raw_float(n) {
                        self.eval_raw_float(value, ctx, out)?;

                        if let Some(reg) = ctx.float_loc.get(n) {
                            let _ = writeln!(out, "    movapd {reg}, xmm0");
                        } else {
                            let _ = writeln!(out, "    movsd [rbp{off}], xmm0");
                        }
                    } else {
                        self.gen_expr(value, ctx, out)?;
                        let _ = writeln!(out, "    mov [rbp{off}], rax");
                    }
                }
                Expr::Field { obj, name } => {
                    self.gen_expr(obj, ctx, out)?;
                    let o = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{o}], rax");
                    self.gen_expr(value, ctx, out)?;
                    let v = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{v}], rax");
                    let lbl = self.add_string(name);
                    let _ = writeln!(out, "    mov rcx, [rbp{o}]");
                    let _ = writeln!(out, "    lea rdx, [rip + {lbl}]");
                    let _ = writeln!(out, "    mov r8, [rbp{v}]");
                    out.push_str("    call lumen_struct_set\n");
                }
                Expr::Index { obj, index } => {
                    self.gen_expr(obj, ctx, out)?;
                    let o = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{o}], rax");
                    self.gen_expr(index, ctx, out)?;
                    let i = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{i}], rax");
                    self.gen_expr(value, ctx, out)?;
                    let v = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{v}], rax");

                    let _ = writeln!(out, "    mov rcx, [rbp{o}]");
                    let _ = writeln!(out, "    mov rdx, [rbp{i}]");
                    let _ = writeln!(out, "    mov r8, [rbp{v}]");
                    out.push_str("    call lumen_index_set\n");
                }
                _ => return Err("native: invalid assignment target".into()),
            },
            Stmt::ExprStmt(e) => {
                self.gen_expr(e, ctx, out)?;
            }
            Stmt::Return(opt) => {

                if let Some(lbl) = ctx.tail_label.clone() {
                    if let Some(Expr::Call { callee, args }) = opt {
                        if matches!(&**callee, Expr::Ident(n) if *n == ctx.func_name)
                            && args.len() == ctx.tail_params.len()
                        {

                            let tparams = ctx.tail_params.clone();
                            let mut tmps = Vec::with_capacity(args.len());
                            // Evaluate all args into temps FIRST, then write them
                            // back into the param slots. Doing it in two passes
                            // avoids clobbering a param we still need to read
                            // (e.g. f(b, a) swapping the two).
                            for (a, (_slot, is_raw)) in args.iter().zip(tparams.iter()) {
                                if *is_raw {
                                    self.eval_raw(a, ctx, out)?;
                                } else {
                                    self.gen_expr(a, ctx, out)?;
                                }
                                let t = self.temp(ctx);
                                let _ = writeln!(out, "    mov [rbp{t}], rax");
                                tmps.push(t);
                            }
                            for (t, (slot, _)) in tmps.iter().zip(tparams.iter()) {
                                let _ = writeln!(out, "    mov rax, [rbp{t}]");
                                let _ = writeln!(out, "    mov [rbp{slot}], rax");
                            }

                            self.emit_gc_poll(out);
                            let _ = writeln!(out, "    jmp {lbl}");
                            return Ok(());
                        }
                    }
                }
                if let Some(e) = opt {
                    if ctx.raw_return {

                        self.eval_raw(e, ctx, out)?;
                    } else {
                        self.gen_expr(e, ctx, out)?;
                    }
                } else if ctx.raw_return {
                    out.push_str("    xor eax, eax\n");
                } else {
                    out.push_str("    call lumen_nil\n");
                }
                out.push_str("    mov rsp, rbp\n    pop rbp\n    ret\n");
            }
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } => {
                let end = self.new_label("ifend");
                let next = self.new_label("ifnext");
                self.gen_cond_false(cond, ctx, out, &next)?;
                for st in then {
                    self.gen_stmt(st, ctx, out)?;
                }
                let _ = writeln!(out, "    jmp {end}\n{next}:");
                for (c, body) in elifs {
                    let n2 = self.new_label("ifnext");
                    self.gen_cond_false(c, ctx, out, &n2)?;
                    for st in body {
                        self.gen_stmt(st, ctx, out)?;
                    }
                    let _ = writeln!(out, "    jmp {end}\n{n2}:");
                }
                if let Some(body) = els {
                    for st in body {
                        self.gen_stmt(st, ctx, out)?;
                    }
                }
                let _ = writeln!(out, "{end}:");
            }
            Stmt::While { cond, body } => {

                // Hoist raw-float accumulators into callee-saved xmm regs for the
                // duration of the loop so the hot body avoids movsd spills. Only
                // safe when the var is never read before its first write each
                // iteration and isn't aliased; flushed back to its slot at loop end.
                let mut promoted_active: Vec<(String, &'static str, i32)> = Vec::new();
                {
                    let mut promoted: Vec<String> = Vec::new();
                    let mut seen: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    for st in body.iter() {
                        if let Stmt::Assign {
                            target: Expr::Ident(v),
                            ..
                        } = st
                        {
                            if ctx.is_raw_float(v)
                                && !ctx.float_loc.contains_key(v)
                                && !seen.contains(v)
                                && !Self::ident_used_in_expr(v, cond)
                                && Self::float_accum_safe(v, body)
                            {
                                seen.insert(v.clone());
                                promoted.push(v.clone());
                            }
                        }
                    }
                    promoted.sort();
                    for v in &promoted {
                        if let Some(reg) = self.acquire_xmm_reg(ctx) {
                            let off = *ctx.locals.get(v).expect("raw-float local has a slot");
                            let _ = writeln!(out, "    movsd xmm0, qword ptr [rbp{off}]");
                            let _ = writeln!(out, "    movapd {reg}, xmm0");
                            ctx.float_loc.insert(v.clone(), reg);
                            promoted_active.push((v.clone(), reg, off));
                        }
                    }
                }

                let top = self.new_label("wtop");
                let end = self.new_label("wend");
                let _ = writeln!(out, "{top}:");
                self.gen_cond_false(cond, ctx, out, &end)?;
                self.emit_gc_poll_cond(body, out);
                ctx.loop_stack.push((top.clone(), end.clone()));
                for st in body {
                    self.gen_stmt(st, ctx, out)?;
                }
                ctx.loop_stack.pop();
                let _ = writeln!(out, "    jmp {top}\n{end}:");

                for (v, reg, off) in &promoted_active {
                    let _ = writeln!(out, "    movsd qword ptr [rbp{off}], {reg}");
                    ctx.float_loc.remove(v);
                }
            }
            Stmt::For { var, iter, body } => match iter {
                Expr::Range { lo, hi } => self.gen_for_range(var, lo, hi, body, ctx, out)?,
                _ => self.gen_for_list(var, iter, body, ctx, out)?,
            },
            Stmt::Break => {
                let end = ctx.loop_stack.last().ok_or("break outside loop")?.1.clone();
                let _ = writeln!(out, "    jmp {end}");
            }
            Stmt::Continue => {
                let cont = ctx
                    .loop_stack
                    .last()
                    .ok_or("continue outside loop")?
                    .0
                    .clone();
                let _ = writeln!(out, "    jmp {cont}");
            }
            Stmt::Raise(e) => {

                self.gen_expr(e, ctx, out)?;
                out.push_str("    mov rcx, rax\n    call lumen_to_str\n");
                out.push_str("    mov rcx, rax\n    call lumen_raise\n");
            }
            Stmt::Try {
                body,
                catch_var,
                catch_body,
            } => {

                let catch_lbl = self.new_label("catch");
                let end_lbl = self.new_label("tryend");
                out.push_str("    call lumen_try_push\n");

                out.push_str("    mov rcx, rax\n");
                out.push_str("    call lumen_setjmp\n");
                out.push_str("    test eax, eax\n");
                let _ = writeln!(out, "    jnz {catch_lbl}");

                for st in body {
                    self.gen_stmt(st, ctx, out)?;
                }

                out.push_str("    call lumen_try_pop\n");
                let _ = writeln!(out, "    jmp {end_lbl}");

                let _ = writeln!(out, "{catch_lbl}:");
                let off = self.slot(ctx, catch_var);
                out.push_str("    call lumen_caught_msg\n");
                let _ = writeln!(out, "    mov [rbp{off}], rax");
                for st in catch_body {
                    self.gen_stmt(st, ctx, out)?;
                }
                let _ = writeln!(out, "{end_lbl}:");
            }
            Stmt::SrcLine(n) => {

                self.cur_line = *n;
                let _ = writeln!(out, "    mov dword ptr [rip + lumen_current_line], {n}");
            }
        }
        Ok(())
    }

    fn gen_for_range(
        &mut self,
        var: &str,
        lo: &Expr,
        hi: &Expr,
        body: &[Stmt],
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {

        let ci = self.acquire_callee_reg(ctx);
        let li = self.acquire_callee_reg(ctx);

        let (counter, limit, use_regs): (String, String, bool) = match (ci, li) {
            (Some(c), Some(l)) => (c.to_string(), l.to_string(), true),
            _ => {
                let rawi = self.temp(ctx);
                let rawhi = self.temp(ctx);
                (format!("[rbp{rawi}]"), format!("[rbp{rawhi}]"), false)
            }
        };

        self.gen_expr(lo, ctx, out)?;
        Self::emit_unbox_int(out);
        let _ = writeln!(out, "    mov {counter}, rax");
        self.gen_expr(hi, ctx, out)?;
        Self::emit_unbox_int(out);
        let _ = writeln!(out, "    mov {limit}, rax");
        let ioff = self.slot(ctx, var);
        let var_raw = ctx.is_raw(var);

        let mut promoted: Vec<String> = Vec::new();
        {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for st in body.iter() {
                if let Stmt::Assign {
                    target: Expr::Ident(v),
                    ..
                } = st
                {
                    if v != var
                        && ctx.is_raw_float(v)
                        && !ctx.float_loc.contains_key(v)
                        && !seen.contains(v)
                        && Self::float_accum_safe(v, body)
                    {
                        seen.insert(v.clone());
                        promoted.push(v.clone());
                    }
                }
            }
        }
        promoted.sort();

        let mut promoted_active: Vec<(String, &'static str, i32)> = Vec::new();
        for v in &promoted {
            if let Some(reg) = self.acquire_xmm_reg(ctx) {
                let off = *ctx.locals.get(v).expect("raw-float local has a slot");

                let _ = writeln!(out, "    movsd xmm0, qword ptr [rbp{off}]");
                let _ = writeln!(out, "    movapd {reg}, xmm0");
                ctx.float_loc.insert(v.clone(), reg);
                promoted_active.push((v.clone(), reg, off));
            }
        }

        // Int accumulators: hoist a raw-int var that is read+written every iteration
        // into a callee-saved GPR for the loop, so the body uses register arithmetic
        // instead of load/store to its slot. Same safety discipline as floats: the
        // var must be raw-int, not the loop var, not already register-mapped, and
        // only ever read-as-value (int_accum_safe). Flushed back to its slot at loop
        // exit. Callee-saved so it survives any call in the body (e.g. an index slow
        // path). Reuses int_loc which the ident/eval_raw paths already honor.
        let mut int_promoted: Vec<String> = Vec::new();
        {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for st in body.iter() {
                if let Stmt::Assign {
                    target: Expr::Ident(v),
                    ..
                } = st
                {
                    if v != var
                        && ctx.is_raw(v)
                        && !ctx.int_loc.contains_key(v)
                        && !seen.contains(v)
                        && Self::int_accum_safe(v, body)
                    {
                        seen.insert(v.clone());
                        int_promoted.push(v.clone());
                    }
                }
            }
        }
        int_promoted.sort();
        let mut int_promoted_active: Vec<(String, &'static str, i32)> = Vec::new();
        for v in &int_promoted {
            if let Some(reg) = self.acquire_callee_reg(ctx) {
                let off = *ctx.locals.get(v).expect("raw-int local has a slot");
                let _ = writeln!(out, "    mov {reg}, [rbp{off}]");
                ctx.int_loc.insert(v.clone(), reg.to_string());
                int_promoted_active.push((v.clone(), reg, off));
            }
        }

        let top = self.new_label("ftop");
        let end = self.new_label("fend");
        let cont = self.new_label("fcont");
        let _ = writeln!(out, "{top}:");

        if use_regs {
            let _ = writeln!(out, "    cmp {counter}, {limit}");
            let _ = writeln!(out, "    jge {end}");
            let _ = writeln!(out, "    mov rax, {counter}");
        } else {
            let _ = writeln!(out, "    mov rax, {counter}");
            let _ = writeln!(out, "    mov rcx, {limit}");
            out.push_str("    cmp rax, rcx\n");
            let _ = writeln!(out, "    jge {end}");
        }

        let mapped_loopvar = var_raw
            && use_regs
            && Self::ident_used_in_body(var, body)
            && !Self::assigned_in_body(var, body);
        if mapped_loopvar {
            ctx.int_loc.insert(var.to_string(), counter.clone());
        } else if Self::ident_used_in_body(var, body) {
            if !var_raw {
                Self::emit_box_int(out);
            }
            let _ = writeln!(out, "    mov [rbp{ioff}], rax");
        }
        self.emit_gc_poll_cond(body, out);
        ctx.loop_stack.push((cont.clone(), end.clone()));
        for st in body {
            self.gen_stmt(st, ctx, out)?;
        }
        ctx.loop_stack.pop();
        if mapped_loopvar {
            ctx.int_loc.remove(var);
        }
        let _ = writeln!(out, "{cont}:");

        if use_regs {
            let _ = writeln!(out, "    add {counter}, 1");
        } else {
            let _ = writeln!(out, "    mov rax, {counter}");
            out.push_str("    add rax, 1\n");
            let _ = writeln!(out, "    mov {counter}, rax");
        }
        let _ = writeln!(out, "    jmp {top}\n{end}:");

        for (v, reg, off) in &promoted_active {
            let _ = writeln!(out, "    movsd qword ptr [rbp{off}], {reg}");
            ctx.float_loc.remove(v);
        }
        for (v, reg, off) in &int_promoted_active {
            let _ = writeln!(out, "    mov [rbp{off}], {reg}");
            ctx.int_loc.remove(v);
        }
        Ok(())
    }

    fn gen_for_list(
        &mut self,
        var: &str,
        iter: &Expr,
        body: &[Stmt],
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {
        self.gen_expr(iter, ctx, out)?;

        out.push_str("    mov rcx, rax\n    call lumen_iter_prep\n");
        let listoff = self.temp(ctx);
        let _ = writeln!(out, "    mov [rbp{listoff}], rax");
        let _ = writeln!(out, "    mov rcx, [rbp{listoff}]");
        out.push_str("    call lumen_len\n");
        let lenoff = self.temp(ctx);
        let _ = writeln!(out, "    mov [rbp{lenoff}], rax");
        let idxoff = self.temp(ctx);
        let _ = writeln!(out, "    mov qword ptr [rbp{idxoff}], 0");
        let _ = self.slot(ctx, var);

        let top = self.new_label("ltop");
        let end = self.new_label("lend");
        let cont = self.new_label("lcont");
        let _ = writeln!(out, "{top}:");

        let _ = writeln!(out, "    mov rax, [rbp{lenoff}]");
        let _ = writeln!(out, "    mov rcx, [rbp{idxoff}]");
        out.push_str("    cmp rcx, rax\n");
        let _ = writeln!(out, "    jge {end}");
        let _ = writeln!(out, "    mov rcx, [rbp{listoff}]");
        let _ = writeln!(out, "    mov rdx, [rbp{idxoff}]");
        out.push_str("    call lumen_list_get\n");
        let voff = *ctx.locals.get(var).unwrap();
        let _ = writeln!(out, "    mov [rbp{voff}], rax");
        self.emit_gc_poll(out);
        ctx.loop_stack.push((cont.clone(), end.clone()));
        for st in body {
            self.gen_stmt(st, ctx, out)?;
        }
        ctx.loop_stack.pop();
        let _ = writeln!(out, "{cont}:");
        let _ = writeln!(out, "    mov rax, [rbp{idxoff}]");
        out.push_str("    add rax, 1\n");
        let _ = writeln!(out, "    mov [rbp{idxoff}], rax");
        let _ = writeln!(out, "    jmp {top}\n{end}:");
        Ok(())
    }

    fn gen_cond_false(
        &mut self,
        cond: &Expr,
        ctx: &mut FnCtx,
        out: &mut String,
        target: &str,
    ) -> Result<(), String> {

        if let Expr::Binary { op, lhs, rhs } = cond {
            let cc_false: Option<&str> = match op {

                BinOp::Lt => Some("jge"),
                BinOp::Le => Some("jg"),
                BinOp::Gt => Some("jle"),
                BinOp::Ge => Some("jl"),
                BinOp::Eq => Some("jne"),
                BinOp::Ne => Some("je"),
                _ => None,
            };
            if let Some(jcc) = cc_false {
                if self.expr_known_int(lhs, ctx) && self.expr_known_int(rhs, ctx) {

                    let lhs_simple = self.simple_raw_operand(lhs, ctx);
                    let rhs_simple = self.simple_raw_operand(rhs, ctx);
                    match (&lhs_simple, &rhs_simple) {
                        (Some(l), Some(r)) => {
                            let _ = writeln!(out, "    mov r8, {l}");
                            let _ = writeln!(out, "    mov r9, {r}");
                        }
                        (_, Some(r)) => {
                            self.eval_raw(lhs, ctx, out)?;
                            out.push_str("    mov r8, rax\n");
                            let _ = writeln!(out, "    mov r9, {r}");
                        }
                        (Some(l), None) => {
                            self.eval_raw(rhs, ctx, out)?;
                            out.push_str("    mov r9, rax\n");
                            let _ = writeln!(out, "    mov r8, {l}");
                        }
                        (None, None) => {
                            self.eval_raw(lhs, ctx, out)?;
                            let lt = self.temp(ctx);
                            let _ = writeln!(out, "    mov [rbp{lt}], rax");
                            self.eval_raw(rhs, ctx, out)?;
                            let _ = writeln!(out, "    mov r9, rax");
                            let _ = writeln!(out, "    mov r8, [rbp{lt}]");
                        }
                    }
                    out.push_str("    cmp r8, r9\n");
                    let _ = writeln!(out, "    {jcc} {target}");
                    return Ok(());
                }
            }
        }
        self.gen_expr(cond, ctx, out)?;
        if Self::expr_is_bool(cond) {

            out.push_str("    test al, 1\n");
            let _ = writeln!(out, "    jz {target}");
            return Ok(());
        }
        out.push_str("    mov rcx, rax\n    call lumen_truthy\n");
        out.push_str("    cmp rax, 0\n");
        let _ = writeln!(out, "    je {target}");
        Ok(())
    }

    fn expr_is_bool(e: &Expr) -> bool {
        match e {
            Expr::Bool(_) => true,
            Expr::Binary { op, .. } => matches!(
                op,
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne
            ),
            Expr::Unary { op: UnOp::Not, .. } => true,
            _ => false,
        }
    }

    fn gen_expr(&mut self, e: &Expr, ctx: &mut FnCtx, out: &mut String) -> Result<(), String> {
        match e {

            Expr::Lambda { .. } => {
                return Err("internal: unlifted lambda in codegen".into());
            }
            Expr::Closure { fn_name, captures } => {
                let f = self
                    .fns
                    .get(fn_name)
                    .ok_or_else(|| self.err(format!("internal: unknown closure '{fn_name}'")))?;

                let total = f.params.len();
                let ncap = captures.len();
                let user_arity = total - ncap;
                let sym = Self::fnsym(fn_name);
                let _ = writeln!(out, "    lea rcx, [rip + {sym}]");
                let _ = writeln!(out, "    mov rdx, {user_arity}");
                let _ = writeln!(out, "    mov r8, {ncap}");
                out.push_str("    call lumen_closure_new\n");

                let coff = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{coff}], rax");
                for (i, cap) in captures.iter().enumerate() {
                    self.gen_expr(cap, ctx, out)?;
                    let _ = writeln!(out, "    mov r8, rax");
                    let _ = writeln!(out, "    mov rdx, {i}");
                    let _ = writeln!(out, "    mov rcx, [rbp{coff}]");
                    out.push_str("    call lumen_closure_set_cap\n");
                }
                let _ = writeln!(out, "    mov rax, [rbp{coff}]");
            }
            Expr::Int(n) => {

                let boxed: u64 = 0x7FF9_0000_0000_0000u64 | ((*n as u64) & 0xFFFF_FFFF_FFFF);
                let _ = writeln!(out, "    mov rax, {boxed}");
            }
            Expr::Float(x) => {

                let bits = x.to_bits();
                let _ = writeln!(out, "    mov rax, {bits}");
                out.push_str("    movq xmm0, rax\n    call lumen_from_double\n");
            }
            Expr::Bool(b) => {

                let boxed: u64 = if *b {
                    0x7FFA_0000_0000_0001
                } else {
                    0x7FFA_0000_0000_0000
                };
                let _ = writeln!(out, "    mov rax, {boxed}");
            }
            Expr::Nil => {

                let _ = writeln!(out, "    mov rax, {}", 0x7FFB_0000_0000_0000u64);
            }
            Expr::Str(s) => {
                let lbl = self.add_string(s);
                let _ = writeln!(out, "    lea rcx, [rip + {lbl}]");
                out.push_str("    call lumen_str_new\n");
            }
            Expr::FStr(parts) => self.gen_fstring(parts, ctx, out)?,
            Expr::Ident(n) => {
                if let Some(reg) = ctx.int_loc.get(n) {

                    let _ = writeln!(out, "    mov rax, {reg}");
                    Self::emit_box_int(out);
                } else if let Some(&off) = ctx.locals.get(n) {
                    let _ = writeln!(out, "    mov rax, [rbp{off}]");
                    if ctx.is_raw(n) {

                        Self::emit_box_int(out);
                    }

                } else if let Some(f) = self.fns.get(n) {

                    let arity = f.params.len();
                    let sym = Self::fnsym(n);
                    let _ = writeln!(out, "    lea rcx, [rip + {sym}]");
                    let _ = writeln!(out, "    mov rdx, {arity}");
                    out.push_str("    call lumen_func_new\n");
                } else {
                    return Err(self.err(format!(
                        "undefined variable '{n}' - is it spelled correctly and in scope?"
                    )));
                }
            }
            Expr::SelfExpr => {
                let off = *ctx.locals.get("self").ok_or("native: self not in scope")?;
                let _ = writeln!(out, "    mov rax, [rbp{off}]");
            }
            Expr::List(elems) => {
                let _ = writeln!(out, "    mov rcx, {}", elems.len());
                out.push_str("    call lumen_list_new\n");
                let loff = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{loff}], rax");
                for el in elems {
                    self.gen_expr(el, ctx, out)?;
                    let _ = writeln!(out, "    mov rdx, rax\n    mov rcx, [rbp{loff}]");
                    out.push_str("    call lumen_list_push\n");
                }
                let _ = writeln!(out, "    mov rax, [rbp{loff}]");
            }

            Expr::ListComp {
                elem,
                var,
                iter,
                cond,
            } => {
                let acc = format!("__lc_acc_{}", self.new_unique());
                out.push_str("    mov rcx, 0\n    call lumen_list_new\n");
                let accoff = self.slot(ctx, &acc);
                let _ = writeln!(out, "    mov [rbp{accoff}], rax");

                let voff = self.slot(ctx, var);
                let saveoff = self.temp(ctx);
                let _ = writeln!(out, "    mov rax, [rbp{voff}]");
                let _ = writeln!(out, "    mov [rbp{saveoff}], rax");
                let push = Stmt::ExprStmt(Expr::Method {
                    obj: Box::new(Expr::Ident(acc.clone())),
                    name: "push".to_string(),
                    args: vec![(**elem).clone()],
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
                    var: var.clone(),
                    iter: (**iter).clone(),
                    body,
                };
                self.gen_stmt(&for_stmt, ctx, out)?;

                let _ = writeln!(out, "    mov rax, [rbp{saveoff}]");
                let _ = writeln!(out, "    mov [rbp{voff}], rax");
                let _ = writeln!(out, "    mov rax, [rbp{accoff}]");
            }
            Expr::Map(entries) => {
                out.push_str("    call lumen_map_new\n");
                let moff = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{moff}], rax");
                for (k, v) in entries {
                    self.gen_expr(k, ctx, out)?;
                    let koff = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{koff}], rax");
                    self.gen_expr(v, ctx, out)?;
                    let _ = writeln!(out, "    mov r8, rax");
                    let _ = writeln!(out, "    mov rdx, [rbp{koff}]");
                    let _ = writeln!(out, "    mov rcx, [rbp{moff}]");
                    out.push_str("    call lumen_map_set\n");
                }
                let _ = writeln!(out, "    mov rax, [rbp{moff}]");
            }
            Expr::Range { lo, hi } => {

                self.gen_range_list(lo, hi, ctx, out)?;
            }

            Expr::IfElse { cond, then, els } => {
                let else_lbl = self.new_label("cond_else");
                let end_lbl = self.new_label("cond_end");
                self.gen_expr(cond, ctx, out)?;
                out.push_str("    mov rcx, rax\n    call lumen_truthy\n    cmp rax, 0\n");
                let _ = writeln!(out, "    je {else_lbl}");
                self.gen_expr(then, ctx, out)?;
                let _ = writeln!(out, "    jmp {end_lbl}");
                let _ = writeln!(out, "{else_lbl}:");
                self.gen_expr(els, ctx, out)?;
                let _ = writeln!(out, "{end_lbl}:");
            }
            Expr::Index { obj, index } => {
                self.gen_expr(obj, ctx, out)?;
                let o = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{o}], rax");
                self.gen_expr(index, ctx, out)?;

                out.push_str("    mov rdx, rax\n");
                let _ = writeln!(out, "    mov rcx, [rbp{o}]");
                out.push_str("    call lumen_index_get\n");
            }

            Expr::Slice { obj, lo, hi } => {
                self.gen_expr(obj, ctx, out)?;
                let o = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{o}], rax");
                match lo {
                    Some(e) => self.gen_expr(e, ctx, out)?,
                    None => out.push_str("    call lumen_nil\n"),
                }
                let lt = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{lt}], rax");
                match hi {
                    Some(e) => self.gen_expr(e, ctx, out)?,
                    None => out.push_str("    call lumen_nil\n"),
                }
                let ht = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{ht}], rax");
                let _ = writeln!(out, "    mov rcx, [rbp{o}]");
                let _ = writeln!(out, "    mov rdx, [rbp{lt}]");
                let _ = writeln!(out, "    mov r8, [rbp{ht}]");
                out.push_str("    call lumen_slice\n");
            }
            Expr::Field { obj, name } => {
                self.gen_expr(obj, ctx, out)?;
                let lbl = self.add_string(name);
                out.push_str("    mov rcx, rax\n");
                let _ = writeln!(out, "    lea rdx, [rip + {lbl}]");
                out.push_str("    call lumen_struct_get\n");
            }
            Expr::Unary { op, expr } => {

                if matches!(op, UnOp::Neg) && self.expr_known_float(expr, ctx) {
                    self.eval_raw_float(e, ctx, out)?;
                    out.push_str("    movq rax, xmm0\n");
                    return Ok(());
                }
                self.gen_expr(expr, ctx, out)?;
                match op {
                    UnOp::Neg => out.push_str("    mov rcx, rax\n    call lumen_neg\n"),
                    UnOp::Not => {
                        out.push_str("    mov rcx, rax\n    call lumen_truthy\n");
                        out.push_str(
                            "    cmp rax, 0\n    sete cl\n    movzx rcx, cl\n    call lumen_bool\n",
                        );
                    }
                }
            }
            Expr::Binary { op, lhs, rhs } => self.gen_binary(*op, lhs, rhs, ctx, out)?,
            Expr::Method { obj, name, args } => self.gen_method(obj, name, args, ctx, out)?,
            Expr::Call { callee, args } => self.gen_call(callee, args, ctx, out)?,
            Expr::NamedCall { callee, args } => self.gen_named_call(callee, args, ctx, out)?,
        }
        Ok(())
    }

    fn gen_range_list(
        &mut self,
        lo: &Expr,
        hi: &Expr,
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {

        out.push_str("    mov rcx, 0\n    call lumen_list_new\n");
        let loff = self.temp(ctx);
        let _ = writeln!(out, "    mov [rbp{loff}], rax");
        self.gen_expr(lo, ctx, out)?;
        out.push_str("    mov rcx, rax\n    call lumen_to_int\n");
        let ioff = self.temp(ctx);
        let _ = writeln!(out, "    mov [rbp{ioff}], rax");
        self.gen_expr(hi, ctx, out)?;
        out.push_str("    mov rcx, rax\n    call lumen_to_int\n");
        let hoff = self.temp(ctx);
        let _ = writeln!(out, "    mov [rbp{hoff}], rax");
        let top = self.new_label("rtop");
        let end = self.new_label("rend");
        let _ = writeln!(out, "{top}:");
        let _ = writeln!(
            out,
            "    mov rax, [rbp{ioff}]\n    mov rcx, [rbp{hoff}]\n    cmp rax, rcx\n    jge {end}"
        );
        let _ = writeln!(out, "    mov rcx, [rbp{ioff}]\n    call lumen_from_int\n    mov rdx, rax\n    mov rcx, [rbp{loff}]\n    call lumen_list_push");
        let _ = writeln!(
            out,
            "    mov rax, [rbp{ioff}]\n    add rax, 1\n    mov [rbp{ioff}], rax\n    jmp {top}"
        );
        let _ = writeln!(out, "{end}:");
        let _ = writeln!(out, "    mov rax, [rbp{loff}]");
        Ok(())
    }

    fn gen_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {

        if matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
        ) && self.expr_known_float(lhs, ctx)
            && self.expr_known_float(rhs, ctx)
        {
            let e = Expr::Binary {
                op,
                lhs: Box::new(lhs.clone()),
                rhs: Box::new(rhs.clone()),
            };
            self.eval_raw_float(&e, ctx, out)?;
            out.push_str("    movq rax, xmm0\n");
            return Ok(());
        }
        if op == BinOp::And || op == BinOp::Or {
            let lbl = self.new_label("sc");
            self.gen_expr(lhs, ctx, out)?;
            let lt = self.temp(ctx);
            let _ = writeln!(out, "    mov [rbp{lt}], rax");
            out.push_str("    mov rcx, rax\n    call lumen_truthy\n    cmp rax, 0\n");
            if op == BinOp::And {

                let _ = writeln!(out, "    je {lbl}");
            } else {

                let _ = writeln!(out, "    jne {lbl}");
            }
            self.gen_expr(rhs, ctx, out)?;
            let _ = writeln!(out, "    jmp {lbl}_end");
            let _ = writeln!(out, "{lbl}:\n    mov rax, [rbp{lt}]\n{lbl}_end:");
            return Ok(());
        }

        let fast_kind: Option<&str> = match op {
            BinOp::Add => Some("add"),
            BinOp::Sub => Some("sub"),
            BinOp::Mul => Some("imul"),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => Some("cmp"),
            _ => None,
        };
        if let Some(kind) = fast_kind {
            if self.expr_known_int(lhs, ctx) && self.expr_known_int(rhs, ctx) {

                let lhs_simple = self.simple_raw_operand(lhs, ctx);
                let rhs_simple = self.simple_raw_operand(rhs, ctx);
                match (&lhs_simple, &rhs_simple) {
                    (Some(l), Some(r)) => {
                        let _ = writeln!(out, "    mov r8, {l}");
                        let _ = writeln!(out, "    mov r9, {r}");
                    }
                    (_, Some(r)) => {
                        self.eval_raw(lhs, ctx, out)?;
                        out.push_str("    mov r8, rax\n");
                        let _ = writeln!(out, "    mov r9, {r}");
                    }
                    (Some(l), None) => {
                        self.eval_raw(rhs, ctx, out)?;
                        out.push_str("    mov r9, rax\n");
                        let _ = writeln!(out, "    mov r8, {l}");
                    }
                    (None, None) => {
                        self.eval_raw(lhs, ctx, out)?;
                        let lt = self.temp(ctx);
                        let _ = writeln!(out, "    mov [rbp{lt}], rax");
                        self.eval_raw(rhs, ctx, out)?;
                        let _ = writeln!(out, "    mov r9, rax");
                        let _ = writeln!(out, "    mov r8, [rbp{lt}]");
                    }
                }
                if kind == "cmp" {
                    out.push_str("    cmp r8, r9\n");
                    let cc = match op {
                        BinOp::Lt => "setl",
                        BinOp::Le => "setle",
                        BinOp::Gt => "setg",
                        BinOp::Ge => "setge",
                        BinOp::Eq => "sete",
                        BinOp::Ne => "setne",
                        _ => unreachable!(),
                    };
                    let _ = writeln!(out, "    {cc} al\n    movzx rax, al");

                    out.push_str("    mov rcx, 0x7FFA000000000000\n    or rax, rcx\n");
                } else {
                    let _ = writeln!(out, "    {kind} r8, r9");
                    out.push_str("    mov rax, r8\n");
                    Self::emit_box_int(out);
                }
                return Ok(());
            }
        }

        self.gen_expr(lhs, ctx, out)?;
        let lt = self.temp(ctx);
        let _ = writeln!(out, "    mov [rbp{lt}], rax");
        self.gen_expr(rhs, ctx, out)?;
        out.push_str("    mov rdx, rax\n");
        let _ = writeln!(out, "    mov rcx, [rbp{lt}]");

        let fast: Option<&str> = match op {
            BinOp::Add => Some("add"),
            BinOp::Sub => Some("sub"),
            BinOp::Mul => Some("imul"),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => Some("cmp"),
            _ => None,
        };
        if let Some(kind) = fast {
            let slow = self.new_label("bin_slow");
            let done = self.new_label("bin_done");

            out.push_str("    mov r8, rcx\n    shr r8, 48\n    cmp r8, 0x7FF9\n");
            let _ = writeln!(out, "    jne {slow}");
            out.push_str("    mov r9, rdx\n    shr r9, 48\n    cmp r9, 0x7FF9\n");
            let _ = writeln!(out, "    jne {slow}");

            out.push_str("    mov r8, rcx\n    shl r8, 16\n    sar r8, 16\n");
            out.push_str("    mov r9, rdx\n    shl r9, 16\n    sar r9, 16\n");
            if kind == "cmp" {
                out.push_str("    cmp r8, r9\n");

                let cc = match op {
                    BinOp::Lt => "setl",
                    BinOp::Le => "setle",
                    BinOp::Gt => "setg",
                    BinOp::Ge => "setge",
                    BinOp::Eq => "sete",
                    BinOp::Ne => "setne",
                    _ => unreachable!(),
                };
                let _ = writeln!(out, "    {cc} al\n    movzx rax, al");

                out.push_str("    mov rcx, 0x7FFA000000000000\n    or rax, rcx\n");
            } else {
                let _ = writeln!(out, "    {kind} r8, r9");
                out.push_str("    mov rax, r8\n");
                Self::emit_box_int(out);
            }
            let _ = writeln!(out, "    jmp {done}");
            let _ = writeln!(out, "{slow}:");

            let _ = writeln!(out, "    mov rcx, [rbp{lt}]");
            let f = Self::runtime_op_name(op);
            let _ = writeln!(out, "    call {f}");
            let _ = writeln!(out, "{done}:");
            return Ok(());
        }

        let f = Self::runtime_op_name(op);
        let _ = writeln!(out, "    call {f}");
        Ok(())
    }

    fn expr_known_int(&self, e: &Expr, ctx: &FnCtx) -> bool {
        match e {
            Expr::Int(_) => true,
            Expr::Ident(n) => self.int_info.is_int_var(&ctx.func_name, n),
            Expr::Unary {
                op: UnOp::Neg,
                expr,
            } => self.expr_known_int(expr, ctx),
            Expr::Binary { op, lhs, rhs } => {
                matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                ) && self.expr_known_int(lhs, ctx)
                    && self.expr_known_int(rhs, ctx)
            }
            Expr::Call { callee, .. } => {
                matches!(&**callee, Expr::Ident(n) if self.int_info.int_ret.contains(n))
            }
            Expr::Index { obj, index } => {
                matches!(&**obj, Expr::Ident(n)
                    if self.int_info.is_int_list_var(&ctx.func_name, n))
                    && self.idx_intish(index, ctx)
            }
            _ => false,
        }
    }

    fn simple_raw_operand(&self, e: &Expr, ctx: &FnCtx) -> Option<String> {
        match e {
            Expr::Int(n) => Some(format!("{n}")),
            Expr::Ident(n) if ctx.is_raw(n) => {

                if let Some(reg) = ctx.int_loc.get(n) {
                    return Some(reg.clone());
                }
                ctx.locals.get(n).map(|off| format!("[rbp{off}]"))
            }
            _ => None,
        }
    }

    // Signed division by a compile-time constant, no idiv. Handles power-of-two
    // divisors with a shift (plus a bias so truncation rounds toward zero, not
    // negative infinity), and everything else via a precomputed magic multiply.
    // Mirrors what the interpreter does so both backends agree bit-for-bit.
    fn emit_const_div(&mut self, d: i64, out: &mut String) {

        if d == 1 {
            return;
        }
        if d == -1 {
            out.push_str("    neg rax\n");
            return;
        }
        let ad = (d as i128).unsigned_abs() as u64;
        if ad.is_power_of_two() {

            let k = ad.trailing_zeros();
            if k == 0 {
                // Dividing by +/-1: the shift is a no-op; sign handled below.
            } else {
                // Add (sign>>(64-k)) before the arithmetic shift so negatives
                // round toward zero to match C/Lumen integer division.
                out.push_str("    mov rcx, rax\n");
                out.push_str("    sar rcx, 63\n");
                let _ = writeln!(out, "    shr rcx, {}", 64 - k);
                out.push_str("    add rax, rcx\n");
                let _ = writeln!(out, "    sar rax, {k}");
            }
            if d < 0 {
                out.push_str("    neg rax\n");
            }
            return;
        }

        let (m, s) = magic_signed(ad);

        // Magic-number division: rax = (n * m) >> (64 + s), reading the high half
        // of the 128-bit imul from rdx. When m is "negative" (top bit set) add n
        // back to correct it, then add the sign bit so the result rounds toward 0.
        out.push_str("    mov rcx, rax\n");
        let _ = writeln!(out, "    movabs rdx, {m}");
        out.push_str("    imul rdx\n");
        out.push_str("    mov rax, rdx\n");

        if m < 0 {
            out.push_str("    add rax, rcx\n");
        }

        if s > 0 {
            let _ = writeln!(out, "    sar rax, {s}");
        }

        out.push_str("    mov rcx, rax\n");
        out.push_str("    shr rcx, 63\n");
        out.push_str("    add rax, rcx\n");

        if d < 0 {
            out.push_str("    neg rax\n");
        }
    }

    fn simple_float_operand(&self, e: &Expr, ctx: &FnCtx) -> Option<String> {
        match e {
            Expr::Ident(n) if ctx.is_raw_float(n) => {

                if let Some(reg) = ctx.float_loc.get(n) {
                    return Some(reg.to_string());
                }
                ctx.locals.get(n).map(|off| format!("[rbp{off}]"))
            }
            _ => None,
        }
    }

    fn fmt_xmm_src(op: &str) -> String {
        if op.starts_with("xmm") {
            op.to_string()
        } else {
            format!("qword ptr {op}")
        }
    }

    fn eval_raw(&mut self, e: &Expr, ctx: &mut FnCtx, out: &mut String) -> Result<(), String> {
        match e {

            Expr::Int(n) => {
                let _ = writeln!(out, "    mov rax, {}", *n);
            }

            Expr::Ident(n) if ctx.is_raw(n) => {

                if let Some(reg) = ctx.int_loc.get(n) {
                    let _ = writeln!(out, "    mov rax, {reg}");
                } else {
                    let off = *ctx.locals.get(n).ok_or("native: raw ident not in scope")?;
                    let _ = writeln!(out, "    mov rax, [rbp{off}]");
                }
            }
            Expr::Unary {
                op: UnOp::Neg,
                expr,
            } if self.expr_known_int(expr, ctx) => {
                self.eval_raw(expr, ctx, out)?;
                out.push_str("    neg rax\n");
            }

            Expr::Binary { op, lhs, rhs }
                if matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                ) && self.expr_known_int(lhs, ctx)
                    && self.expr_known_int(rhs, ctx) =>
            {

                if matches!(op, BinOp::Div | BinOp::Mod) {
                    if let Expr::Int(d) = &**rhs {
                        let d = *d;
                        if d != 0 {

                            self.eval_raw(lhs, ctx, out)?;
                            if matches!(op, BinOp::Mod) {
                                // n % 2^k by masking, no idiv/imul. Bias by the
                                // sign so the remainder truncates toward zero
                                // (matches interp wrapping_rem). Guard: positive
                                // power-of-two with d-1 in imm32.
                                let ad = (d as i128).unsigned_abs() as u64;
                                if d > 0 && ad.is_power_of_two() && d <= (1i64 << 31) {
                                    let k = ad.trailing_zeros();
                                    let mask = d - 1;
                                    out.push_str("    mov rcx, rax\n");
                                    out.push_str("    sar rcx, 63\n");
                                    let _ = writeln!(out, "    shr rcx, {}", 64 - k);
                                    out.push_str("    add rax, rcx\n");
                                    let _ = writeln!(out, "    and rax, {mask}");
                                    out.push_str("    sub rax, rcx\n");
                                    return Ok(());
                                }
                                let nt = self.temp(ctx);
                                let _ = writeln!(out, "    mov [rbp{nt}], rax");
                                self.emit_const_div(d, out);

                                let _ = writeln!(out, "    movabs rcx, {d}");
                                out.push_str("    imul rax, rcx\n");
                                out.push_str("    mov rcx, rax\n");
                                let _ = writeln!(out, "    mov rax, [rbp{nt}]");
                                out.push_str("    sub rax, rcx\n");
                            } else {
                                self.emit_const_div(d, out);
                            }
                            return Ok(());
                        }

                    }
                }

                if !matches!(op, BinOp::Div | BinOp::Mod) {

                    if let Some(rhs_op) = self.simple_raw_operand(rhs, ctx) {
                        self.eval_raw(lhs, ctx, out)?;

                        let imm32 = match &**rhs {
                            Expr::Int(n) if i32::try_from(*n).is_ok() => Some(*n),
                            _ => None,
                        };
                        match (op, imm32) {
                            (BinOp::Add, Some(n)) => {
                                let _ = writeln!(out, "    add rax, {n}");
                            }
                            (BinOp::Sub, Some(n)) => {
                                let _ = writeln!(out, "    sub rax, {n}");
                            }
                            (BinOp::Mul, Some(n)) => {
                                let _ = writeln!(out, "    imul rax, rax, {n}");
                            }
                            _ => {
                                let _ = writeln!(out, "    mov rcx, {rhs_op}");
                                match op {
                                    BinOp::Add => out.push_str("    add rax, rcx\n"),
                                    BinOp::Sub => out.push_str("    sub rax, rcx\n"),
                                    BinOp::Mul => out.push_str("    imul rax, rcx\n"),
                                    _ => unreachable!(),
                                }
                            }
                        }
                        return Ok(());
                    }

                    if let Some(lhs_op) = self.simple_raw_operand(lhs, ctx) {
                        self.eval_raw(rhs, ctx, out)?;
                        match op {
                            BinOp::Add => {
                                let _ = writeln!(out, "    mov rcx, {lhs_op}");
                                out.push_str("    add rax, rcx\n");
                            }
                            BinOp::Mul => {
                                let _ = writeln!(out, "    mov rcx, {lhs_op}");
                                out.push_str("    imul rax, rcx\n");
                            }
                            BinOp::Sub => {
                                out.push_str("    mov rcx, rax\n");
                                let _ = writeln!(out, "    mov rax, {lhs_op}");
                                out.push_str("    sub rax, rcx\n");
                            }
                            _ => unreachable!(),
                        }
                        return Ok(());
                    }
                }

                self.eval_raw(lhs, ctx, out)?;
                let lt = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{lt}], rax");
                self.eval_raw(rhs, ctx, out)?;
                match op {
                    BinOp::Add => {
                        let _ = writeln!(out, "    mov rcx, [rbp{lt}]");
                        out.push_str("    add rax, rcx\n");
                    }
                    BinOp::Sub => {
                        out.push_str("    mov rcx, rax\n");
                        let _ = writeln!(out, "    mov rax, [rbp{lt}]");
                        out.push_str("    sub rax, rcx\n");
                    }
                    BinOp::Mul => {
                        let _ = writeln!(out, "    mov rcx, [rbp{lt}]");
                        out.push_str("    imul rax, rcx\n");
                    }

                    BinOp::Div | BinOp::Mod => {

                        // Variable divisor: branch on zero. The zero path re-boxes
                        // and calls the runtime so it can raise the same error the
                        // interpreter would; the nonzero path does a plain idiv.
                        let nz = self.new_label("divnz");
                        let done = self.new_label("divdone");
                        out.push_str("    test rax, rax\n");
                        let _ = writeln!(out, "    jne {nz}");

                        Self::emit_box_int(out);
                        out.push_str("    mov rdx, rax\n");
                        let _ = writeln!(out, "    mov rax, [rbp{lt}]");
                        Self::emit_box_int(out);
                        out.push_str("    mov rcx, rax\n");
                        let f = Self::runtime_op_name(*op);
                        let _ = writeln!(out, "    call {f}");
                        Self::emit_unbox_int(out);
                        let _ = writeln!(out, "    jmp {done}");

                        let _ = writeln!(out, "{nz}:");
                        out.push_str("    mov rcx, rax\n");
                        let _ = writeln!(out, "    mov rax, [rbp{lt}]");
                        out.push_str("    cqo\n    idiv rcx\n");
                        if matches!(op, BinOp::Mod) {
                            out.push_str("    mov rax, rdx\n");
                        }
                        let _ = writeln!(out, "{done}:");
                    }
                    _ => unreachable!(),
                }
            }

            Expr::Binary {
                op: BinOp::Pow,
                lhs,
                rhs,
            } if self.expr_known_int(lhs, ctx)
                && matches!(&**rhs, Expr::Int(e) if (0..=8).contains(e)) =>
            {
                let exp = match &**rhs {
                    Expr::Int(e) => *e,
                    _ => unreachable!(),
                };
                if exp == 0 {
                    out.push_str("    mov rax, 1\n");
                } else {

                    self.eval_raw(lhs, ctx, out)?;
                    let bt = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{bt}], rax");

                    for _ in 1..exp {
                        let _ = writeln!(out, "    mov rcx, [rbp{bt}]");
                        out.push_str("    imul rax, rcx\n");
                    }
                }
            }

            // Int-list unboxed read: bounds-checked inline load + unbox, skipping
            // the lumen_index_get dispatch. Mirror of the float-list movsd path but
            // the element is a NaN-boxed int, so we shl/sar-unbox instead. The slow
            // path (negative/oob index) falls back to the runtime so it raises the
            // same error and stays byte-identical.
            Expr::Index { obj, index }
                if matches!(&**obj, Expr::Ident(n)
                    if self.int_info.is_int_list_var(&ctx.func_name, n))
                    && self.idx_intish(index, ctx) =>
            {
                self.gen_expr(obj, ctx, out)?;
                let objt = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{objt}], rax");

                self.eval_raw(index, ctx, out)?;
                let idxt = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{idxt}], rax");

                let slow = self.new_label("ilidx_slow");
                let done = self.new_label("ilidx_done");

                let _ = writeln!(out, "    mov r8, [rbp{objt}]");
                out.push_str("    mov r11, 0xFFFFFFFFFFFF\n    and r8, r11\n");

                let _ = writeln!(out, "    mov r9, [rbp{idxt}]");
                out.push_str("    test r9, r9\n");
                let _ = writeln!(out, "    js {slow}");
                out.push_str("    cmp r9, qword ptr [r8+16]\n");
                let _ = writeln!(out, "    jae {slow}");

                // items pointer at +32; element is a NaN-boxed int -> unbox into rax.
                out.push_str("    mov r8, qword ptr [r8+32]\n");
                out.push_str("    mov rax, qword ptr [r8+r9*8]\n");
                Self::emit_unbox_int(out);
                let _ = writeln!(out, "    jmp {done}");

                let _ = writeln!(out, "{slow}:");
                let _ = writeln!(out, "    mov rax, [rbp{idxt}]");
                Self::emit_box_int(out);
                out.push_str("    mov rdx, rax\n");
                let _ = writeln!(out, "    mov rcx, [rbp{objt}]");
                out.push_str("    call lumen_index_get\n");
                Self::emit_unbox_int(out);
                let _ = writeln!(out, "{done}:");
            }

            Expr::Call { callee, args }
                if matches!(&**callee, Expr::Ident(n)
                    if self.int_info.int_ret.contains(n)) =>
            {
                if let Expr::Ident(n) = &**callee {
                    if self.raw_entry_fns.contains(n)
                        && self
                            .fns
                            .get(n)
                            .is_some_and(|f| f.params.len() == args.len())
                    {
                        self.emit_raw_call(n, args, ctx, out)?;
                        return Ok(());
                    }
                }
                self.gen_expr(e, ctx, out)?;
                Self::emit_unbox_int(out);
            }

            _ => {
                self.gen_expr(e, ctx, out)?;
                Self::emit_unbox_int(out);
            }
        }
        Ok(())
    }

    fn expr_known_float(&self, e: &Expr, ctx: &FnCtx) -> bool {
        match e {
            Expr::Float(_) => true,
            Expr::Ident(n) => self.int_info.is_float_var(&ctx.func_name, n),
            Expr::Unary {
                op: UnOp::Neg,
                expr,
            } => self.expr_known_float(expr, ctx),
            Expr::Binary { op, lhs, rhs } => {
                matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                ) && self.expr_known_float(lhs, ctx)
                    && self.expr_known_float(rhs, ctx)
            }
            Expr::Call { callee, .. } => {
                matches!(&**callee, Expr::Ident(n) if self.int_info.float_ret.contains(n))
            }

            Expr::Index { obj, index } => {
                matches!(&**obj, Expr::Ident(n)
                    if self.int_info.is_float_list_var(&ctx.func_name, n))
                    && self.idx_intish(index, ctx)
            }
            _ => false,
        }
    }

    fn idx_intish(&self, e: &Expr, ctx: &FnCtx) -> bool {
        match e {
            Expr::Int(_) => true,
            Expr::Ident(n) => self.int_info.is_int_var(&ctx.func_name, n) || ctx.is_raw(n),
            Expr::Binary { op, lhs, rhs } => {
                matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                ) && self.idx_intish(lhs, ctx)
                    && self.idx_intish(rhs, ctx)
            }
            _ => false,
        }
    }

    fn eval_raw_float(
        &mut self,
        e: &Expr,
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {
        match e {

            Expr::Float(x) => {
                let lbl = self.add_double(x.to_bits());
                let _ = writeln!(out, "    movsd xmm0, qword ptr [rip + {lbl}]");
            }

            Expr::Ident(n) if ctx.is_raw_float(n) => {

                if let Some(reg) = ctx.float_loc.get(n) {
                    let _ = writeln!(out, "    movapd xmm0, {reg}");
                } else {
                    let off = *ctx
                        .locals
                        .get(n)
                        .ok_or("native: raw-float ident not in scope")?;
                    let _ = writeln!(out, "    movsd xmm0, qword ptr [rbp{off}]");
                }
            }

            Expr::Unary {
                op: UnOp::Neg,
                expr,
            } if self.expr_known_float(expr, ctx) => {
                self.eval_raw_float(expr, ctx, out)?;
                out.push_str("    movq rax, xmm0\n");
                out.push_str("    mov rcx, 0x8000000000000000\n    xor rax, rcx\n");
                out.push_str("    movq xmm0, rax\n");
            }

            Expr::Binary { op, lhs, rhs }
                if matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                ) && self.expr_known_float(lhs, ctx)
                    && self.expr_known_float(rhs, ctx) =>
            {

                if !matches!(op, BinOp::Mod) {

                    if let Some(rhs_op) = self.simple_float_operand(rhs, ctx) {
                        self.eval_raw_float(lhs, ctx, out)?;
                        let ins = match op {
                            BinOp::Add => "addsd",
                            BinOp::Sub => "subsd",
                            BinOp::Mul => "mulsd",
                            BinOp::Div => "divsd",
                            _ => unreachable!(),
                        };
                        let _ = writeln!(out, "    {} xmm0, {}", ins, Self::fmt_xmm_src(&rhs_op));
                        return Ok(());
                    }

                    if let Some(lhs_op) = self.simple_float_operand(lhs, ctx) {
                        match op {
                            BinOp::Add | BinOp::Mul => {
                                self.eval_raw_float(rhs, ctx, out)?;
                                let ins = if matches!(op, BinOp::Add) {
                                    "addsd"
                                } else {
                                    "mulsd"
                                };
                                let _ = writeln!(
                                    out,
                                    "    {} xmm0, {}",
                                    ins,
                                    Self::fmt_xmm_src(&lhs_op)
                                );
                            }
                            BinOp::Sub | BinOp::Div => {

                                self.eval_raw_float(rhs, ctx, out)?;
                                out.push_str("    movapd xmm1, xmm0\n");

                                if lhs_op.starts_with("xmm") {
                                    let _ = writeln!(out, "    movapd xmm0, {lhs_op}");
                                } else {
                                    let _ = writeln!(out, "    movsd xmm0, qword ptr {lhs_op}");
                                }
                                let ins = if matches!(op, BinOp::Sub) {
                                    "subsd"
                                } else {
                                    "divsd"
                                };
                                let _ = writeln!(out, "    {ins} xmm0, xmm1");
                            }
                            _ => unreachable!(),
                        }
                        return Ok(());
                    }
                }

                self.eval_raw_float(lhs, ctx, out)?;
                let lt = self.temp(ctx);
                let _ = writeln!(out, "    movsd qword ptr [rbp{lt}], xmm0");
                self.eval_raw_float(rhs, ctx, out)?;
                out.push_str("    movapd xmm1, xmm0\n");
                let _ = writeln!(out, "    movsd xmm0, qword ptr [rbp{lt}]");
                match op {
                    BinOp::Add => out.push_str("    addsd xmm0, xmm1\n"),
                    BinOp::Sub => out.push_str("    subsd xmm0, xmm1\n"),
                    BinOp::Mul => out.push_str("    mulsd xmm0, xmm1\n"),
                    BinOp::Div => out.push_str("    divsd xmm0, xmm1\n"),
                    BinOp::Mod => {

                        out.push_str("    movapd xmm2, xmm0\n");
                        out.push_str("    divsd xmm0, xmm1\n");
                        out.push_str("    cvttsd2si rax, xmm0\n");
                        out.push_str("    cvtsi2sd xmm0, rax\n");
                        out.push_str("    mulsd xmm0, xmm1\n");
                        out.push_str("    subsd xmm2, xmm0\n");
                        out.push_str("    movapd xmm0, xmm2\n");
                    }
                    _ => unreachable!(),
                }
            }

            Expr::Index { obj, index }
                if matches!(&**obj, Expr::Ident(n)
                    if self.int_info.is_float_list_var(&ctx.func_name, n))
                    && self.idx_intish(index, ctx) =>
            {

                self.gen_expr(obj, ctx, out)?;
                let objt = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{objt}], rax");

                self.eval_raw(index, ctx, out)?;
                let idxt = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{idxt}], rax");

                let slow = self.new_label("flidx_slow");
                let done = self.new_label("flidx_done");

                let _ = writeln!(out, "    mov r8, [rbp{objt}]");
                out.push_str("    mov r11, 0xFFFFFFFFFFFF\n    and r8, r11\n");

                let _ = writeln!(out, "    mov r9, [rbp{idxt}]");
                out.push_str("    test r9, r9\n");
                let _ = writeln!(out, "    js {slow}");
                out.push_str("    cmp r9, qword ptr [r8+16]\n");
                let _ = writeln!(out, "    jae {slow}");

                out.push_str("    mov r8, qword ptr [r8+32]\n");
                out.push_str("    movsd xmm0, qword ptr [r8+r9*8]\n");
                let _ = writeln!(out, "    jmp {done}");

                let _ = writeln!(out, "{slow}:");
                let _ = writeln!(out, "    mov rax, [rbp{idxt}]");
                Self::emit_box_int(out);
                out.push_str("    mov rdx, rax\n");
                let _ = writeln!(out, "    mov rcx, [rbp{objt}]");
                out.push_str("    call lumen_index_get\n");
                out.push_str("    movq xmm0, rax\n");
                let _ = writeln!(out, "{done}:");
            }

            Expr::Call { callee, .. }
                if matches!(&**callee, Expr::Ident(n)
                    if self.int_info.float_ret.contains(n)) =>
            {
                self.gen_expr(e, ctx, out)?;
                out.push_str("    movq xmm0, rax\n");
            }

            _ => {
                self.gen_expr(e, ctx, out)?;
                out.push_str("    movq xmm0, rax\n");
            }
        }
        Ok(())
    }

    fn runtime_op_name(op: BinOp) -> &'static str {
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
            BinOp::And | BinOp::Or => unreachable!(),
        }
    }

    fn gen_fstring(
        &mut self,
        parts: &[FStrPart],
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {

        out.push_str("    lea rcx, [rip + .empty_str]\n    call lumen_str_new\n");
        self.want_empty_str();
        let acc = self.temp(ctx);
        let _ = writeln!(out, "    mov [rbp{acc}], rax");
        for p in parts {
            match p {
                FStrPart::Lit(l) => {
                    let lbl = self.add_string(l);
                    let _ = writeln!(out, "    lea rcx, [rip + {lbl}]");
                    out.push_str("    call lumen_str_new\n");
                }
                FStrPart::Expr(e) => {
                    self.gen_expr(e, ctx, out)?;
                    out.push_str("    mov rcx, rax\n    call lumen_to_str\n");
                }
            }
            out.push_str("    mov rdx, rax\n");
            let _ = writeln!(out, "    mov rcx, [rbp{acc}]");
            out.push_str("    call lumen_str_concat\n");
            let _ = writeln!(out, "    mov [rbp{acc}], rax");
        }
        let _ = writeln!(out, "    mov rax, [rbp{acc}]");
        Ok(())
    }

    fn want_empty_str(&mut self) {
        if !self.field_tables.contains(".empty_str:") {
            self.field_tables.push_str(".empty_str: .asciz \"\"\n");
        }
    }

    fn gen_method(
        &mut self,
        obj: &Expr,
        name: &str,
        args: &[Expr],
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {

        if let Expr::Ident(m) = obj {
            if crate::builtins::is_module(m) {
                let bf = crate::builtins::lookup(m, name)
                    .ok_or_else(|| format!("native {m}: no fn '{name}'"))?;

                if bf.arity == 1 {
                    self.gen_expr(&args[0], ctx, out)?;
                    out.push_str("    mov rcx, rax\n");
                } else if bf.arity == 2 {
                    self.gen_expr(&args[0], ctx, out)?;
                    let t = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{t}], rax");
                    self.gen_expr(&args[1], ctx, out)?;
                    out.push_str("    mov rdx, rax\n");
                    let _ = writeln!(out, "    mov rcx, [rbp{t}]");
                } else if bf.arity == 3 {
                    self.gen_expr(&args[0], ctx, out)?;
                    let t0 = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{t0}], rax");
                    self.gen_expr(&args[1], ctx, out)?;
                    let t1 = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{t1}], rax");
                    self.gen_expr(&args[2], ctx, out)?;
                    out.push_str("    mov r8, rax\n");
                    let _ = writeln!(out, "    mov rdx, [rbp{t1}]");
                    let _ = writeln!(out, "    mov rcx, [rbp{t0}]");
                } else if bf.arity == 4 {
                    self.gen_expr(&args[0], ctx, out)?;
                    let t0 = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{t0}], rax");
                    self.gen_expr(&args[1], ctx, out)?;
                    let t1 = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{t1}], rax");
                    self.gen_expr(&args[2], ctx, out)?;
                    let t2 = self.temp(ctx);
                    let _ = writeln!(out, "    mov [rbp{t2}], rax");
                    self.gen_expr(&args[3], ctx, out)?;
                    out.push_str("    mov r9, rax\n");
                    let _ = writeln!(out, "    mov r8, [rbp{t2}]");
                    let _ = writeln!(out, "    mov rdx, [rbp{t1}]");
                    let _ = writeln!(out, "    mov rcx, [rbp{t0}]");
                }
                let _ = writeln!(out, "    call {}", bf.symbol);
                return Ok(());
            }
        }
        if name == "len" && args.is_empty() {
            self.gen_expr(obj, ctx, out)?;
            out.push_str(
                "    mov rcx, rax\n    call lumen_len\n    mov rcx, rax\n    call lumen_from_int\n",
            );
            return Ok(());
        }
        if name == "push" {
            self.gen_expr(obj, ctx, out)?;
            let o = self.temp(ctx);
            let _ = writeln!(out, "    mov [rbp{o}], rax");
            for a in args {
                self.gen_expr(a, ctx, out)?;
                let _ = writeln!(
                    out,
                    "    mov rdx, rax\n    mov rcx, [rbp{o}]\n    call lumen_list_push"
                );
            }
            out.push_str("    call lumen_nil\n");
            return Ok(());
        }

        if name == "reverse" || name == "sort" {
            let rt = if name == "reverse" {
                "lumen_list_reverse"
            } else {
                "lumen_list_sort"
            };
            self.gen_expr(obj, ctx, out)?;
            out.push_str("    mov rcx, rax\n");
            let _ = writeln!(out, "    call {rt}");
            out.push_str("    call lumen_nil\n");
            return Ok(());
        }
        let one_arg_method: Option<&str> = match name {
            "upper" => Some("lumen_str_upper"),
            "lower" => Some("lumen_str_lower"),
            "trim" => Some("lumen_str_trim"),
            "title" => Some("lumen_str_title"),
            "lstrip" => Some("lumen_str_lstrip"),
            "rstrip" => Some("lumen_str_rstrip"),
            "pop" => Some("lumen_list_pop"),
            "keys" => Some("lumen_map_keys"),
            "values" => Some("lumen_map_values"),
            _ => None,
        };
        if let Some(rt) = one_arg_method {
            self.gen_expr(obj, ctx, out)?;
            out.push_str("    mov rcx, rax\n");
            let _ = writeln!(out, "    call {rt}");
            return Ok(());
        }

        if name == "get" && !args.is_empty() {
            self.gen_expr(obj, ctx, out)?;
            let r = self.temp(ctx);
            let _ = writeln!(out, "    mov [rbp{r}], rax");
            self.gen_expr(&args[0], ctx, out)?;
            let k = self.temp(ctx);
            let _ = writeln!(out, "    mov [rbp{k}], rax");
            if args.len() > 1 {
                self.gen_expr(&args[1], ctx, out)?;
                let _ = writeln!(out, "    mov r8, rax");
                let _ = writeln!(out, "    mov rdx, [rbp{k}]");
                let _ = writeln!(out, "    mov rcx, [rbp{r}]");
                out.push_str("    call lumen_map_get_or\n");
            } else {
                let _ = writeln!(out, "    mov rdx, [rbp{k}]");
                let _ = writeln!(out, "    mov rcx, [rbp{r}]");
                out.push_str("    call lumen_map_get\n");
            }
            return Ok(());
        }

        let two_arg_method: Option<&str> = match name {
            "split" => Some("lumen_str_split"),
            "contains" => Some("lumen_contains"),
            "starts_with" => Some("lumen_str_starts_with"),
            "ends_with" => Some("lumen_str_ends_with"),
            "find" => Some("lumen_str_find"),
            "replace" => Some("lumen_str_replace"),
            "repeat" => Some("lumen_str_repeat"),
            "join" => Some("lumen_join"),
            "has" => Some("lumen_map_has"),
            "remove" => Some("lumen_map_remove"),
            "insert" => Some("lumen_list_insert"),
            "index" => Some("lumen_list_index"),
            "count" => Some("lumen_list_count"),
            _ => None,
        };
        let two_arg_method = match two_arg_method {
            Some(rt) if name == "insert" && args.len() == 2 => Some(rt),
            Some(rt) if name == "replace" && args.len() == 2 => Some(rt),
            Some(rt) if name != "insert" && name != "replace" && args.len() == 1 => Some(rt),
            _ => None,
        };
        if let Some(rt) = two_arg_method {

            self.gen_expr(obj, ctx, out)?;
            let r = self.temp(ctx);
            let _ = writeln!(out, "    mov [rbp{r}], rax");
            if name == "insert" {

                self.gen_expr(&args[0], ctx, out)?;
                out.push_str("    mov rcx, rax\n    call lumen_to_int\n");
                let idx = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{idx}], rax");
                self.gen_expr(&args[1], ctx, out)?;
                let v = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{v}], rax");
                let _ = writeln!(out, "    mov rcx, [rbp{r}]");
                let _ = writeln!(out, "    mov rdx, [rbp{idx}]");
                let _ = writeln!(out, "    mov r8, [rbp{v}]");
                out.push_str("    call lumen_list_insert\n    call lumen_nil\n");
                return Ok(());
            }
            if name == "replace" {

                self.gen_expr(&args[0], ctx, out)?;
                let oldv = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{oldv}], rax");
                self.gen_expr(&args[1], ctx, out)?;
                let newv = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{newv}], rax");
                let _ = writeln!(out, "    mov rcx, [rbp{r}]");
                let _ = writeln!(out, "    mov rdx, [rbp{oldv}]");
                let _ = writeln!(out, "    mov r8, [rbp{newv}]");
                out.push_str("    call lumen_str_replace\n");
                return Ok(());
            }
            self.gen_expr(&args[0], ctx, out)?;
            out.push_str("    mov rdx, rax\n");
            let _ = writeln!(out, "    mov rcx, [rbp{r}]");
            let _ = writeln!(out, "    call {rt}");
            return Ok(());
        }

        let sname = self.resolve_method(obj, name, ctx);
        if let Some(sn) = sname {
            let sym = Self::methodsym(&sn, name);
            self.gen_expr(obj, ctx, out)?;
            let recv = self.temp(ctx);
            let _ = writeln!(out, "    mov [rbp{recv}], rax");
            let mut slots = vec![recv];
            for a in args {
                self.gen_expr(a, ctx, out)?;
                let t = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{t}], rax");
                slots.push(t);
            }
            self.emit_win64_call(&sym, &slots, out);
            return Ok(());
        }
        Err(self.err(format!(
            "cannot resolve method '{name}' - no method or builtin with that name applies here"
        )))
    }

    fn resolve_method(&self, obj: &Expr, name: &str, ctx: &FnCtx) -> Option<String> {

        if matches!(obj, Expr::SelfExpr) {
            if let Some(h) = &ctx.struct_hint {
                if self.methods.contains_key(&(h.clone(), name.to_string())) {
                    return Some(h.clone());
                }
            }
        }

        let matches: Vec<&(String, String)> =
            self.methods.keys().filter(|(_, m)| m == name).collect();
        if matches.len() == 1 {
            return Some(matches[0].0.clone());
        }
        None
    }

    fn gen_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {
        let name = match callee {
            Expr::Ident(n) => n.clone(),

            other => return self.gen_indirect_call(other, args, ctx, out),
        };

        if ctx.locals.contains_key(&name) && !self.fns.contains_key(&name) {
            return self.gen_indirect_call(callee, args, ctx, out);
        }

        match name.as_str() {
            "print" => {
                if args.is_empty() {
                    // print() -> blank line.
                    out.push_str("    call lumen_print_nl\n    call lumen_nil\n");
                    return Ok(());
                }
                if args.len() == 1 {
                    self.gen_expr(&args[0], ctx, out)?;
                    out.push_str("    mov rcx, rax\n    call lumen_print\n    call lumen_nil\n");
                    return Ok(());
                }
                // N args: each value, space-separated, then one newline 
                // matches the interpreter's parts.join(" ") + newline.
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str("    call lumen_print_space\n");
                    }
                    self.gen_expr(a, ctx, out)?;
                    out.push_str("    mov rcx, rax\n    call lumen_print_part\n");
                }
                out.push_str("    call lumen_print_nl\n    call lumen_nil\n");
                return Ok(());
            }
            "len" => {
                self.gen_expr(&args[0], ctx, out)?;
                out.push_str("    mov rcx, rax\n    call lumen_len\n    mov rcx, rax\n    call lumen_from_int\n");
                return Ok(());
            }
            "str" => {
                self.gen_expr(&args[0], ctx, out)?;
                out.push_str("    mov rcx, rax\n    call lumen_to_str\n");
                return Ok(());
            }
            "int" => {
                self.gen_expr(&args[0], ctx, out)?;
                out.push_str("    mov rcx, rax\n    call lumen_to_int_val\n");
                return Ok(());
            }
            "float" => {
                self.gen_expr(&args[0], ctx, out)?;
                out.push_str("    mov rcx, rax\n    call lumen_to_float_val\n");
                return Ok(());
            }

            "sum" | "min" | "max" | "abs" | "round" | "type" | "ord" | "chr" | "is_digit"
            | "is_alpha" | "is_space" => {
                let rt = match name.as_str() {
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
                    "is_space" => "lumen_is_space",
                    _ => unreachable!(),
                };
                self.gen_expr(&args[0], ctx, out)?;
                out.push_str("    mov rcx, rax\n");
                let _ = writeln!(out, "    call {rt}");
                return Ok(());
            }

            "input" => {
                if let Some(a) = args.first() {
                    self.gen_expr(a, ctx, out)?;
                    out.push_str("    mov rcx, rax\n");
                } else {
                    out.push_str("    call lumen_nil\n    mov rcx, rax\n");
                }
                out.push_str("    call lumen_input\n");
                return Ok(());
            }
            "assert" => {
                self.gen_expr(&args[0], ctx, out)?;
                out.push_str("    mov rcx, rax\n    call lumen_assert\n    call lumen_nil\n");
                return Ok(());
            }

            "drop" => {
                self.gen_expr(&args[0], ctx, out)?;
                out.push_str("    mov rcx, rax\n    call lumen_release\n    call lumen_nil\n");
                return Ok(());
            }

            "range" => {
                if args.len() == 1 {
                    return self.gen_range_list(&Expr::Int(0), &args[0], ctx, out);
                } else if args.len() == 2 {
                    return self.gen_range_list(&args[0], &args[1], ctx, out);
                }
                return Err("range() takes 1 or 2 args".into());
            }
            _ => {}
        }

        if self.structs.contains_key(&name) {
            return self.gen_struct_ctor(
                &name,
                &positional_to_named(&name, args, &self.structs)?,
                ctx,
                out,
            );
        }

        if let Some(ef) = self.externs.get(&name).cloned() {
            return self.gen_ffi(&name, &ef, args, ctx, out);
        }

        if self.fns.contains_key(&name) {

            if args.len() == 1 {
                self.gen_expr(&args[0], ctx, out)?;
                out.push_str("    mov rcx, rax\n");
                let _ = writeln!(out, "    call {}", Self::fnsym(&name));
                return Ok(());
            }
            let mut slots = Vec::new();
            for a in args {
                self.gen_expr(a, ctx, out)?;
                let t = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{t}], rax");
                slots.push(t);
            }
            self.emit_win64_call(&Self::fnsym(&name), &slots, out);
            return Ok(());
        }
        Err(self.err(format!(
            "undefined function '{name}' - no such function, method, or builtin"
        )))
    }

    fn gen_indirect_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {
        if args.len() > 3 {
            return Err("native: indirect call supports up to 3 args".into());
        }

        self.gen_expr(callee, ctx, out)?;
        let fslot = self.temp(ctx);
        let _ = writeln!(out, "    mov [rbp{fslot}], rax");
        let mut slots = Vec::new();
        for a in args {
            self.gen_expr(a, ctx, out)?;
            let t = self.temp(ctx);
            let _ = writeln!(out, "    mov [rbp{t}], rax");
            slots.push(t);
        }

        let regs = ["rdx", "r8", "r9"];
        let _ = writeln!(out, "    mov rcx, [rbp{fslot}]");
        for (i, s) in slots.iter().enumerate() {
            let _ = writeln!(out, "    mov {}, [rbp{}]", regs[i], s);
        }
        let _ = writeln!(out, "    call lumen_call{}", slots.len());
        Ok(())
    }

    fn emit_win64_call(&self, sym: &str, slots: &[i32], out: &mut String) {
        let regs = ["rcx", "rdx", "r8", "r9"];
        let n = slots.len();
        if n <= 4 {
            for (i, sl) in slots.iter().enumerate() {
                let _ = writeln!(out, "    mov {}, [rbp{}]", regs[i], sl);
            }
            let _ = writeln!(out, "    call {sym}");
            return;
        }

        let stack_args = n - 4;
        // Beyond 4 args, Win64 passes them on the stack above the 32-byte shadow
        // space. Pad the reservation to keep rsp 16-aligned at the call.
        let mut bytes = 32 + stack_args * 8;
        if bytes % 16 != 0 {
            bytes += 8;
        }
        let _ = writeln!(out, "    sub rsp, {bytes}");

        #[allow(clippy::needless_range_loop)]
        for i in 4..n {
            let dst = 32 + (i - 4) * 8;
            let _ = writeln!(out, "    mov rax, [rbp{}]", slots[i]);
            let _ = writeln!(out, "    mov [rsp+{dst}], rax");
        }

        for i in 0..4 {
            let _ = writeln!(out, "    mov {}, [rbp{}]", regs[i], slots[i]);
        }
        let _ = writeln!(out, "    call {sym}");
        let _ = writeln!(out, "    add rsp, {bytes}");
    }

    fn emit_raw_call(
        &mut self,
        name: &str,
        args: &[Expr],
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {
        let regs = ["rcx", "rdx", "r8", "r9"];

        if args.len() == 1 {
            self.eval_raw(&args[0], ctx, out)?;
            out.push_str("    mov rcx, rax\n");
            let _ = writeln!(out, "    call {}", Self::fnsym_raw(name));
            return Ok(());
        }
        let mut slots = Vec::with_capacity(args.len());
        for a in args {
            self.eval_raw(a, ctx, out)?;
            let t = self.temp(ctx);
            let _ = writeln!(out, "    mov [rbp{t}], rax");
            slots.push(t);
        }
        let sym = Self::fnsym_raw(name);
        let n = slots.len();
        if n <= 4 {
            for (i, sl) in slots.iter().enumerate() {
                let _ = writeln!(out, "    mov {}, [rbp{}]", regs[i], sl);
            }
            let _ = writeln!(out, "    call {sym}");
            return Ok(());
        }
        let stack_args = n - 4;
        let mut bytes = 32 + stack_args * 8;
        if bytes % 16 != 0 {
            bytes += 8;
        }
        let _ = writeln!(out, "    sub rsp, {bytes}");
        #[allow(clippy::needless_range_loop)]
        for i in 4..n {
            let dst = 32 + (i - 4) * 8;
            let _ = writeln!(out, "    mov rax, [rbp{}]", slots[i]);
            let _ = writeln!(out, "    mov [rsp+{dst}], rax");
        }
        for i in 0..4 {
            let _ = writeln!(out, "    mov {}, [rbp{}]", regs[i], slots[i]);
        }
        let _ = writeln!(out, "    call {sym}");
        let _ = writeln!(out, "    add rsp, {bytes}");
        Ok(())
    }

    fn mir_all_int(mir: &crate::mir::MirFn) -> bool {
        use crate::mir::Inst;
        if mir.ret_is_float || mir.param_is_float.iter().any(|&f| f) {
            return false;
        }
        for b in &mir.blocks {
            for i in &b.insts {
                match i {
                    Inst::Bin { op, .. } if op.is_float() => return false,
                    Inst::Un {
                        op: crate::mir::UnKind::FNeg,
                        ..
                    } => return false,
                    Inst::Call { is_float, .. } if *is_float => return false,
                    crate::mir::Inst::Move { src, .. } => {
                        if matches!(src, crate::mir::Val::FloatConst(_)) {
                            return false;
                        }
                    }
                    _ => {}
                }
            }
        }
        true
    }

    fn gen_fn_mir_body(
        &mut self,
        mir: &crate::mir::MirFn,
        ra: &crate::mir::RegAlloc,
        f: &FnDef,
        ctx: &mut FnCtx,
    ) -> Result<String, String> {
        use crate::mir::{Inst, UnKind};
        use std::collections::HashMap;

        let mut vslot: HashMap<u32, i32> = HashMap::new();

        let mut pslot: HashMap<u32, i32> = HashMap::new();
        for (i, p) in f.params.iter().enumerate() {
            let off = *ctx
                .locals
                .get(&p.name)
                .ok_or_else(|| format!("mir: param {} has no slot", p.name))?;
            pslot.insert(i as u32, off);
        }

        let mut blabel: HashMap<u32, String> = HashMap::new();
        for b in &mir.blocks {
            blabel.insert(b.id, self.new_label("mb"));
        }

        fn store_dst(
            ra: &crate::mir::RegAlloc,
            vslot: &mut HashMap<u32, i32>,
            ctx: &mut FnCtx,
            out: &mut String,
            v: u32,
        ) {
            if let Some(reg) = ra.reg_of(v) {
                let _ = writeln!(out, "    mov {reg}, rax");
            } else {
                let s = if let Some(&s) = vslot.get(&v) {
                    s
                } else {
                    ctx.stack_size += 8;
                    let s = -ctx.stack_size;
                    vslot.insert(v, s);
                    s
                };
                let _ = writeln!(out, "    mov [rbp{s}], rax");
            }
        }

        let mut out = String::new();

        for b in &mir.blocks {
            let _ = writeln!(out, "{}:", blabel[&b.id]);
            for inst in &b.insts {
                match inst {

                    Inst::Phi { .. } => {}
                    Inst::Move { dst, src } => {
                        self.mir_load_val(src, "rax", ra, &mut vslot, &pslot, ctx, &mut out);
                        store_dst(ra, &mut vslot, ctx, &mut out, *dst);
                    }
                    Inst::Un { dst, op, a, wrap } => {
                        self.mir_load_val(a, "rax", ra, &mut vslot, &pslot, ctx, &mut out);
                        match op {
                            UnKind::INeg => out.push_str("    neg rax\n"),
                            UnKind::FNeg => unreachable!("int-only MIR has no FNeg"),
                        }
                        if *wrap {
                            Self::emit_unbox_int(&mut out);
                        }
                        store_dst(ra, &mut vslot, ctx, &mut out, *dst);
                    }
                    Inst::Bin {
                        dst,
                        op,
                        a,
                        b: bb,
                        wrap,
                    } => {
                        self.mir_emit_bin(
                            *op, a, bb, *wrap, *dst, mir, ra, &mut vslot, &pslot, ctx, &mut out,
                        );
                    }
                    Inst::Call {
                        dst, callee, args, ..
                    } => {

                        let regs = ["rcx", "rdx", "r8", "r9"];
                        let mut argslots = Vec::with_capacity(args.len());
                        for a in args {
                            self.mir_load_val(a, "rax", ra, &mut vslot, &pslot, ctx, &mut out);
                            let t = self.temp(ctx);
                            let _ = writeln!(out, "    mov [rbp{t}], rax");
                            argslots.push(t);
                        }
                        let sym = Self::fnsym_raw(callee);
                        let n = argslots.len();
                        if n <= 4 {
                            for (i, sl) in argslots.iter().enumerate() {
                                let _ = writeln!(out, "    mov {}, [rbp{}]", regs[i], sl);
                            }
                            let _ = writeln!(out, "    call {sym}");
                        } else {
                            let stack_args = n - 4;
                            let mut bytes = 32 + stack_args * 8;
                            if bytes % 16 != 0 {
                                bytes += 8;
                            }
                            let _ = writeln!(out, "    sub rsp, {bytes}");
                            #[allow(clippy::needless_range_loop)]
                            for i in 4..n {
                                let dstoff = 32 + (i - 4) * 8;
                                let _ = writeln!(out, "    mov rax, [rbp{}]", argslots[i]);
                                let _ = writeln!(out, "    mov [rsp+{dstoff}], rax");
                            }
                            for i in 0..4 {
                                let _ = writeln!(out, "    mov {}, [rbp{}]", regs[i], argslots[i]);
                            }
                            let _ = writeln!(out, "    call {sym}");
                            let _ = writeln!(out, "    add rsp, {bytes}");
                        }

                        store_dst(ra, &mut vslot, ctx, &mut out, *dst);
                    }
                    Inst::Ret(opt) => {
                        if let Some(v) = opt {
                            self.mir_load_val(v, "rax", ra, &mut vslot, &pslot, ctx, &mut out);
                        } else {
                            out.push_str("    xor eax, eax\n");
                        }

                        out.push_str("    mov rsp, rbp\n    pop rbp\n    ret\n");
                    }
                    Inst::Jmp(t) => {
                        self.mir_edge_phis(b.id, *t, mir, ra, &mut vslot, &pslot, ctx, &mut out);
                        let _ = writeln!(out, "    jmp {}", blabel[t]);
                    }
                    Inst::Br { cond, t, f: fb } => {

                        self.mir_load_val(cond, "rax", ra, &mut vslot, &pslot, ctx, &mut out);
                        out.push_str("    test rax, rax\n");
                        let tramp_t = self.new_label("mbt");
                        let tramp_f = self.new_label("mbf");
                        let _ = writeln!(out, "    jne {tramp_t}");
                        let _ = writeln!(out, "    jmp {tramp_f}");
                        let _ = writeln!(out, "{tramp_t}:");
                        self.mir_edge_phis(b.id, *t, mir, ra, &mut vslot, &pslot, ctx, &mut out);
                        let _ = writeln!(out, "    jmp {}", blabel[t]);
                        let _ = writeln!(out, "{tramp_f}:");
                        self.mir_edge_phis(b.id, *fb, mir, ra, &mut vslot, &pslot, ctx, &mut out);
                        let _ = writeln!(out, "    jmp {}", blabel[fb]);
                    }
                }
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn mir_edge_phis(
        &mut self,
        from: u32,
        to: u32,
        mir: &crate::mir::MirFn,
        ra: &crate::mir::RegAlloc,
        vslot: &mut std::collections::HashMap<u32, i32>,
        pslot: &std::collections::HashMap<u32, i32>,
        ctx: &mut FnCtx,
        out: &mut String,
    ) {
        use crate::mir::Inst;
        let dst_block = mir.block(to);

        let mut moves: Vec<(u32, crate::mir::Val)> = Vec::new();
        for inst in &dst_block.insts {
            if let Inst::Phi { dst, srcs } = inst {
                if let Some((_, v)) = srcs.iter().find(|(pb, _)| *pb == from) {
                    moves.push((*dst, *v));
                }
            }
        }
        if moves.is_empty() {
            return;
        }

        let mut temps = Vec::with_capacity(moves.len());
        for (_, v) in &moves {
            self.mir_load_val(v, "rax", ra, vslot, pslot, ctx, out);
            let t = self.temp(ctx);
            let _ = writeln!(out, "    mov [rbp{t}], rax");
            temps.push(t);
        }
        for ((dst, _), t) in moves.iter().zip(temps.iter()) {
            let _ = writeln!(out, "    mov rax, [rbp{t}]");
            if let Some(reg) = ra.reg_of(*dst) {
                let _ = writeln!(out, "    mov {reg}, rax");
            } else {
                let d = if let Some(&s) = vslot.get(dst) {
                    s
                } else {
                    ctx.stack_size += 8;
                    let s = -ctx.stack_size;
                    vslot.insert(*dst, s);
                    s
                };
                let _ = writeln!(out, "    mov [rbp{d}], rax");
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn mir_load_val(
        &mut self,
        v: &crate::mir::Val,
        reg: &str,
        ra: &crate::mir::RegAlloc,
        vslot: &mut std::collections::HashMap<u32, i32>,
        pslot: &std::collections::HashMap<u32, i32>,
        ctx: &mut FnCtx,
        out: &mut String,
    ) {
        use crate::mir::Val;
        match v {
            Val::IntConst(n) => {
                let _ = writeln!(out, "    movabs {reg}, {n}");
            }
            Val::FloatConst(_) => unreachable!("int-only MIR has no float consts"),
            Val::Param(i) => {
                let off = pslot[i];
                let _ = writeln!(out, "    mov {reg}, [rbp{off}]");
            }
            Val::Vreg(r) => {
                if let Some(phys) = ra.reg_of(*r) {
                    if phys != reg {
                        let _ = writeln!(out, "    mov {reg}, {phys}");
                    }
                    return;
                }
                let off = if let Some(&s) = vslot.get(r) {
                    s
                } else {

                    ctx.stack_size += 8;
                    let s = -ctx.stack_size;
                    vslot.insert(*r, s);
                    s
                };
                let _ = writeln!(out, "    mov {reg}, [rbp{off}]");
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn mir_emit_bin(
        &mut self,
        op: crate::mir::BinKind,
        a: &crate::mir::Val,
        b: &crate::mir::Val,
        wrap: bool,
        dst: u32,
        mir: &crate::mir::MirFn,
        ra: &crate::mir::RegAlloc,
        vslot: &mut std::collections::HashMap<u32, i32>,
        pslot: &std::collections::HashMap<u32, i32>,
        ctx: &mut FnCtx,
        out: &mut String,
    ) {
        use crate::mir::BinKind::*;

        let store =
            |out: &mut String, vslot: &mut std::collections::HashMap<u32, i32>, ctx: &mut FnCtx| {
                if let Some(reg) = ra.reg_of(dst) {
                    let _ = writeln!(out, "    mov {reg}, rax");
                } else {
                    let d = if let Some(&s) = vslot.get(&dst) {
                        s
                    } else {
                        ctx.stack_size += 8;
                        let s = -ctx.stack_size;
                        vslot.insert(dst, s);
                        s
                    };
                    let _ = writeln!(out, "    mov [rbp{d}], rax");
                }
            };

        match op {
            IAdd | ISub | IMul => {
                self.mir_load_val(a, "rax", ra, vslot, pslot, ctx, out);
                let at = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{at}], rax");
                self.mir_load_val(b, "rcx", ra, vslot, pslot, ctx, out);
                let _ = writeln!(out, "    mov rax, [rbp{at}]");
                match op {
                    IAdd => out.push_str("    add rax, rcx\n"),
                    ISub => out.push_str("    sub rax, rcx\n"),
                    IMul => out.push_str("    imul rax, rcx\n"),
                    _ => unreachable!(),
                }
                if wrap {
                    Self::emit_unbox_int(out);
                }
                store(out, vslot, ctx);
            }
            DivConst(d) => {

                self.mir_load_val(a, "rax", ra, vslot, pslot, ctx, out);
                self.emit_const_div(d, out);
                store(out, vslot, ctx);
            }
            ModConst(d) => {

                self.mir_load_val(a, "rax", ra, vslot, pslot, ctx, out);
                let nt = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{nt}], rax");
                self.emit_const_div(d, out);
                let _ = writeln!(out, "    movabs rcx, {d}");
                out.push_str("    imul rax, rcx\n");
                out.push_str("    mov rcx, rax\n");
                let _ = writeln!(out, "    mov rax, [rbp{nt}]");
                out.push_str("    sub rax, rcx\n");
                store(out, vslot, ctx);
            }
            IDiv | IMod => {

                self.mir_load_val(a, "rax", ra, vslot, pslot, ctx, out);
                let nt = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{nt}], rax");
                self.mir_load_val(b, "rax", ra, vslot, pslot, ctx, out);
                if let Some(line) = mir.div_lines.get(&dst) {
                    let _ = writeln!(out, "    mov dword ptr [rip + lumen_current_line], {line}");
                }
                let nz = self.new_label("mdivnz");
                let done = self.new_label("mdivdone");
                out.push_str("    test rax, rax\n");
                let _ = writeln!(out, "    jne {nz}");

                Self::emit_box_int(out);
                out.push_str("    mov rdx, rax\n");
                let _ = writeln!(out, "    mov rax, [rbp{nt}]");
                Self::emit_box_int(out);
                out.push_str("    mov rcx, rax\n");
                let runtime = if matches!(op, IDiv) {
                    "lumen_div"
                } else {
                    "lumen_mod"
                };
                let _ = writeln!(out, "    call {runtime}");
                Self::emit_unbox_int(out);
                let _ = writeln!(out, "    jmp {done}");

                let _ = writeln!(out, "{nz}:");
                out.push_str("    mov rcx, rax\n");
                let _ = writeln!(out, "    mov rax, [rbp{nt}]");
                out.push_str("    cqo\n    idiv rcx\n");
                if matches!(op, IMod) {
                    out.push_str("    mov rax, rdx\n");
                }
                let _ = writeln!(out, "{done}:");
                store(out, vslot, ctx);
            }
            IEq | INe | ILt | ILe | IGt | IGe => {
                self.mir_load_val(a, "rax", ra, vslot, pslot, ctx, out);
                let at = self.temp(ctx);
                let _ = writeln!(out, "    mov [rbp{at}], rax");
                self.mir_load_val(b, "rcx", ra, vslot, pslot, ctx, out);
                let _ = writeln!(out, "    mov rax, [rbp{at}]");
                out.push_str("    cmp rax, rcx\n");
                let cc = match op {
                    IEq => "sete",
                    INe => "setne",
                    ILt => "setl",
                    ILe => "setle",
                    IGt => "setg",
                    IGe => "setge",
                    _ => unreachable!(),
                };
                let _ = writeln!(out, "    {cc} al");
                out.push_str("    movzx rax, al\n");
                store(out, vslot, ctx);
            }

            FAdd | FSub | FMul | FDiv | FEq | FNe | FLt | FLe | FGt | FGe => {
                unreachable!("int-only MIR emitter reached a float op")
            }
        }
    }

    fn gen_named_call(
        &mut self,
        callee: &Expr,
        args: &[(String, Expr)],
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {
        let name = match callee {
            Expr::Ident(n) => n.clone(),
            _ => return Err(self.err("a constructor call must name a struct type")),
        };
        if !self.structs.contains_key(&name) {
            return Err(self.err(format!("no struct type named '{name}'")));
        }
        self.gen_struct_ctor(&name, args, ctx, out)
    }

    fn gen_struct_ctor(
        &mut self,
        name: &str,
        args: &[(String, Expr)],
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {
        let sd = self.structs.get(name).cloned().unwrap();

        let name_lbl = self.add_string(name);
        let table_lbl = format!(".fields_{name}");
        if !self.field_tables.contains(&format!("{table_lbl}:")) {
            let mut entries = Vec::new();
            for f in &sd.fields {
                let fl = self.add_string(&f.name);
                entries.push(fl);
            }
            let _ = writeln!(
                self.field_tables,
                "{}: .quad {}",
                table_lbl,
                entries
                    .iter()
                    .map(|e| e.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let _ = writeln!(out, "    lea rcx, [rip + {name_lbl}]");
        let _ = writeln!(out, "    mov rdx, {}", sd.fields.len());
        let _ = writeln!(out, "    lea r8, [rip + {table_lbl}]");
        out.push_str("    call lumen_struct_new\n");
        let soff = self.temp(ctx);
        let _ = writeln!(out, "    mov [rbp{soff}], rax");
        for (fname, fexpr) in args {
            self.gen_expr(fexpr, ctx, out)?;
            let v = self.temp(ctx);
            let _ = writeln!(out, "    mov [rbp{v}], rax");
            let fl = self.add_string(fname);
            let _ = writeln!(out, "    mov rcx, [rbp{soff}]");
            let _ = writeln!(out, "    lea rdx, [rip + {fl}]");
            let _ = writeln!(out, "    mov r8, [rbp{v}]");
            out.push_str("    call lumen_struct_set\n");
        }
        let _ = writeln!(out, "    mov rax, [rbp{soff}]");
        Ok(())
    }

    fn gen_ffi(
        &mut self,
        _name: &str,
        ef: &ExternFn,
        args: &[Expr],
        ctx: &mut FnCtx,
        out: &mut String,
    ) -> Result<(), String> {

        if args.len() > 16 {
            return Err(self.err("FFI: at most 16 arguments are supported"));
        }

        let is_float: Vec<bool> = (0..args.len())
            .map(|i| {
                ef.params
                    .get(i)
                    .map(|p| matches!(&p.ty, Type::Named(n) if n == "f64" || n == "f32"))
                    .unwrap_or(false)
            })
            .collect();

        let mut slots = Vec::new();
        for (i, a) in args.iter().enumerate() {
            self.gen_expr(a, ctx, out)?;
            if is_float[i] {

                out.push_str("    mov rcx, rax\n    call lumen_ffi_argdouble\n");
            } else {
                out.push_str("    mov rcx, rax\n    call lumen_ffi_argint\n");
            }
            let t = self.temp(ctx);
            let _ = writeln!(out, "    mov [rbp{t}], rax");
            slots.push(t);
        }
        let gp = ["rcx", "rdx", "r8", "r9"];
        let xmm = ["xmm0", "xmm1", "xmm2", "xmm3"];

        let stack_args = slots.len().saturating_sub(4);
        if stack_args > 0 {

            let bytes = 32 + stack_args * 8;
            let bytes = (bytes + 15) & !15;
            let _ = writeln!(out, "    sub rsp, {bytes}");
            for (i, sl) in slots.iter().enumerate().skip(4) {
                let off = 32 + (i - 4) * 8;
                let _ = writeln!(out, "    mov rax, [rbp{sl}]");
                let _ = writeln!(out, "    mov [rsp+{off}], rax");
            }
            for (i, sl) in slots.iter().enumerate().take(4) {
                if is_float[i] {
                    let _ = writeln!(out, "    movq {}, [rbp{}]", xmm[i], sl);
                } else {
                    let _ = writeln!(out, "    mov {}, [rbp{}]", gp[i], sl);
                }
            }
            let _ = writeln!(out, "    call {}", ef.name);
            let _ = writeln!(out, "    add rsp, {bytes}");
        } else {
            for (i, sl) in slots.iter().enumerate() {
                if is_float[i] {
                    let _ = writeln!(out, "    movq {}, [rbp{}]", xmm[i], sl);
                } else {
                    let _ = writeln!(out, "    mov {}, [rbp{}]", gp[i], sl);
                }
            }

            let _ = writeln!(out, "    call {}", ef.name);
        }

        match &ef.ret {
            Type::Nil => out.push_str("    call lumen_nil\n"),
            Type::Named(n) if n == "f64" || n == "f32" => {

                out.push_str("    call lumen_from_double\n");
            }
            _ => out.push_str("    mov rcx, rax\n    call lumen_from_int\n"),
        }
        Ok(())
    }
}

fn positional_to_named(
    name: &str,
    args: &[Expr],
    structs: &HashMap<String, StructDef>,
) -> Result<Vec<(String, Expr)>, String> {
    let sd = structs.get(name).unwrap();
    if args.len() > sd.fields.len() {
        return Err(format!(
            "too many arguments for struct '{name}': it has {} field(s) but {} were given",
            sd.fields.len(),
            args.len()
        ));
    }
    Ok(sd
        .fields
        .iter()
        .zip(args.iter())
        .map(|(f, a)| (f.name.clone(), a.clone()))
        .collect())
}

fn escape(s: &str) -> String {
    let mut esc = String::new();
    for c in s.bytes() {
        match c {
            b'"' => esc.push_str("\\\""),
            b'\\' => esc.push_str("\\\\"),
            b'\n' => esc.push_str("\\n"),
            b'\t' => esc.push_str("\\t"),
            b'\r' => esc.push_str("\\r"),
            0x20..=0x7e => esc.push(c as char),
            other => {
                let _ = write!(esc, "\\{other:03o}");
            }
        }
    }
    esc
}

// Computes the magic multiplier and shift for signed division by a constant,
// the classic Granlund-Montgomery / "Hacker's Delight" algorithm. Returns
// (m, s) such that  n / d  ==  high64(n * m) adjusted by shift s. emit_const_div
// turns these into instructions; both must agree with the interpreter exactly.
fn magic_signed(d_abs: u64) -> (i64, u32) {

    const W: u32 = 64;
    let two31: u64 = 1u64 << (W - 1);
    let ad = d_abs;

    let t = two31;
    let anc = t - 1 - t.rem_euclid(ad);
    let mut p: u32 = W - 1;
    let mut q1 = two31 / anc;
    let mut r1 = two31 - q1 * anc;
    let mut q2 = two31 / ad;
    let mut r2 = two31 - q2 * ad;
    loop {
        p += 1;
        q1 = q1.wrapping_mul(2);
        r1 = r1.wrapping_mul(2);
        if r1 >= anc {
            q1 = q1.wrapping_add(1);
            r1 = r1.wrapping_sub(anc);
        }
        q2 = q2.wrapping_mul(2);
        r2 = r2.wrapping_mul(2);
        if r2 >= ad {
            q2 = q2.wrapping_add(1);
            r2 = r2.wrapping_sub(ad);
        }
        let delta = ad - r2;
        if !(q1 < delta || (q1 == delta && r1 == 0)) {
            break;
        }
    }
    let m = q2.wrapping_add(1);
    let s = p - W;
    (m as i64, s)
}

// Tiny peephole pass: collapse "store rax to slot; reload that same slot" into a
// single store (and a reg move if the reload targeted a different register).
// We emit a lot of these store/reload pairs naively, so this is a cheap cleanup.
fn peephole(asm: &str) -> String {
    let lines: Vec<&str> = asm.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let cur = lines[i];

        if i + 1 < lines.len() {
            if let Some((mem, _)) = parse_store_rax(cur) {
                let next = lines[i + 1];
                if let Some((dst, src_mem)) = parse_mov_reg_mem(next) {
                    if src_mem == mem {
                        out.push(cur.to_string());
                        if dst == "rax" {

                        } else {

                            out.push(format!("    mov {dst}, rax"));
                        }
                        i += 2;
                        continue;
                    }
                }
            }
        }
        out.push(cur.to_string());
        i += 1;
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

fn parse_store_rax(line: &str) -> Option<(String, String)> {
    let t = line.trim();
    let rest = t.strip_prefix("mov ")?;
    let (lhs, rhs) = rest.split_once(", ")?;
    if rhs == "rax" && lhs.starts_with('[') && lhs.ends_with(']') {
        Some((lhs.to_string(), "rax".to_string()))
    } else {
        None
    }
}

fn parse_mov_reg_mem(line: &str) -> Option<(String, String)> {
    let t = line.trim();
    let rest = t.strip_prefix("mov ")?;
    let (lhs, rhs) = rest.split_once(", ")?;

    if !lhs.starts_with('[') && rhs.starts_with('[') && rhs.ends_with(']') && is_gpr(lhs) {
        Some((lhs.to_string(), rhs.to_string()))
    } else {
        None
    }
}

fn is_gpr(s: &str) -> bool {
    matches!(
        s,
        "rax"
            | "rbx"
            | "rcx"
            | "rdx"
            | "rsi"
            | "rdi"
            | "rbp"
            | "rsp"
            | "r8"
            | "r9"
            | "r10"
            | "r11"
            | "r12"
            | "r13"
            | "r14"
            | "r15"
    )
}

#[cfg(test)]
mod peephole_tests {
    use super::peephole;

    #[test]
    fn drops_redundant_reload() {
        let asm = "    mov [rbp-8], rax\n    mov rax, [rbp-8]\n";
        let got = peephole(asm);
        assert!(got.contains("mov [rbp-8], rax"));

        assert!(!got.contains("mov rax, [rbp-8]"));
    }

    #[test]
    fn reload_to_reg_move() {
        let asm = "    mov [rbp-8], rax\n    mov rcx, [rbp-8]\n";
        let got = peephole(asm);
        assert!(got.contains("mov rcx, rax"));
        assert!(!got.contains("mov rcx, [rbp-8]"));
    }

    #[test]
    fn leaves_unrelated_alone() {

        let asm = "    mov [rbp-8], rax\n    mov rcx, [rbp-16]\n";
        let got = peephole(asm);
        assert!(got.contains("mov rcx, [rbp-16]"));
    }
}

#[cfg(test)]
mod magic_div_tests {
    use super::magic_signed;

    fn modeled_quotient(n: i64, d: i64) -> i64 {
        if d == 1 {
            return n;
        }
        if d == -1 {
            return n.wrapping_neg();
        }
        let ad = (d as i128).unsigned_abs() as u64;
        if ad.is_power_of_two() {
            let k = ad.trailing_zeros();

            let sign = (n >> 63) as u64;
            let corr = (sign >> (64 - k)) as i64;
            let mut q = n.wrapping_add(corr);
            q >>= k;
            if d < 0 {
                q = q.wrapping_neg();
            }
            return q;
        }
        let (m, s) = magic_signed(ad);

        let hi = (((n as i128) * (m as i128)) >> 64) as i64;
        let mut q = hi;
        if m < 0 {
            q = q.wrapping_add(n);
        }
        if s > 0 {
            q >>= s;
        }
        let signbit = ((q as u64) >> 63) as i64;
        q = q.wrapping_add(signbit);
        if d < 0 {
            q = q.wrapping_neg();
        }
        q
    }

    fn modeled_rem(n: i64, d: i64) -> i64 {
        let q = modeled_quotient(n, d);
        n.wrapping_sub(q.wrapping_mul(d))
    }

    #[test]
    fn magic_matches_idiv() {
        let divisors: [i64; 30] = [
            1, -1, 2, -2, 3, -3, 4, -4, 5, 6, 7, -7, 8, -8, 9, 10, 11, 13, -13, 16, 17, 100, -100,
            128, 1000, -1000, 1024, 65536, 1_000_000, 7_777_777,
        ];

        let mut dividends: Vec<i64> = vec![
            0,
            1,
            -1,
            2,
            -2,
            6,
            7,
            8,
            -6,
            -7,
            -8,
            13,
            -13,
            41,
            -41,
            100,
            -100,
            99999,
            -99999,
            140737488355327,
            -140737488355328,
            i64::MAX,
            i64::MIN,
            i64::MAX - 1,
            i64::MIN + 1,
            123456789,
            -123456789,
        ];

        let mut x: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..2000 {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            dividends.push(x as i64);
        }
        for &d in &divisors {
            for &n in &dividends {

                if n == i64::MIN && d == -1 {
                    continue;
                }
                let want_q = n.wrapping_div(d);
                let got_q = modeled_quotient(n, d);
                assert_eq!(got_q, want_q, "QUOTIENT mismatch n={} d={}", n, d);
                let want_r = n.wrapping_rem(d);
                let got_r = modeled_rem(n, d);
                assert_eq!(got_r, want_r, "REMAINDER mismatch n={} d={}", n, d);
            }
        }
    }

    // Models the exact instruction sequence emitted for `n % 2^k` (positive
    // power-of-two divisor): bias by sign, mask, subtract bias. Must equal
    // wrapping_rem so the masked path stays byte-identical to interp.
    fn modeled_mask_rem(n: i64, d: i64) -> i64 {
        let k = (d as u64).trailing_zeros();
        let rcx = ((n >> 63) as u64 >> (64 - k)) as i64; // sar 63; shr 64-k
        let r = n.wrapping_add(rcx) & (d - 1); // add; and mask
        r.wrapping_sub(rcx) // sub bias
    }

    #[test]
    fn mask_mod_matches_rem() {
        let pow2: [i64; 12] = [2, 4, 8, 16, 32, 64, 128, 256, 1024, 65536, 1 << 20, 1 << 30];
        let mut ns: Vec<i64> = vec![
            0, 1, -1, 2, -2, 3, -3, 7, -7, 8, -8, 15, -15, 100, -100, i64::MAX, i64::MIN,
            140737488355327, -140737488355328,
        ];
        let mut x: u64 = 0x1234_5678_9ABC_DEF1;
        for _ in 0..3000 {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ns.push(x as i64);
        }
        for &d in &pow2 {
            for &n in &ns {
                assert_eq!(
                    modeled_mask_rem(n, d),
                    n.wrapping_rem(d),
                    "mask % mismatch n={} d={}",
                    n,
                    d
                );
            }
        }
    }
}
