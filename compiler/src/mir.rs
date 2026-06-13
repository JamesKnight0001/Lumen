//! SSA mid-level IR for the numeric fast path, plus a linear-scan register allocator.
//!
//! Only functions with a purely i64/f64 signature and a numeric-scalar body are
//! lowered here (see mir_eligible); everything else stays on the slow interpreter
//! path. The lowerer builds SSA on the fly (Braun et al. style: read_var /
//! write_var with on-demand, trivially-removable phis), and regalloc assigns
//! x86-64 registers so the interp and native backends produce byte-identical results.

use crate::ast::{Expr, FnDef, Stmt, Type};
use crate::types::IntInfo;

// Gate deciding whether a function may take the MIR fast path. Anything not
// provably numeric-scalar here falls back to the interpreter, so this must only
// admit shapes the lowerer can actually handle.
fn all_numsig(f: &FnDef) -> bool {
    let is_num = |t: &Type| matches!(t, Type::Named(n) if n == "i64" || n == "f64");
    !f.is_method && f.params.iter().all(|p| is_num(&p.ty)) && is_num(&f.ret)
}

pub fn mir_eligible(f: &FnDef, info: &IntInfo) -> bool {
    let _ = info;
    if !all_numsig(f) {
        return false;
    }
    f.body.iter().all(stmt_ok)
}

fn stmt_ok(s: &Stmt) -> bool {
    match s {
        Stmt::Let { value, .. } => expr_ok(value),
        Stmt::Assign { target, value } => matches!(target, Expr::Ident(_)) && expr_ok(value),
        Stmt::ExprStmt(e) => expr_ok(e),
        Stmt::Return(opt) => opt.as_ref().map(expr_ok).unwrap_or(true),
        Stmt::If {
            cond,
            then,
            elifs,
            els,
        } => {
            expr_ok(cond)
                && then.iter().all(stmt_ok)
                && elifs
                    .iter()
                    .all(|(c, b)| expr_ok(c) && b.iter().all(stmt_ok))
                && els
                    .as_ref()
                    .map(|b| b.iter().all(stmt_ok))
                    .unwrap_or(true)
        }
        Stmt::While { cond, body } => expr_ok(cond) && body.iter().all(stmt_ok),

        Stmt::For { iter, body, .. } => {
            matches!(iter, Expr::Range { .. }) && body.iter().all(stmt_ok)
        }

        Stmt::Try { .. } | Stmt::Raise(_) => false,
        Stmt::Break | Stmt::Continue | Stmt::SrcLine(_) => true,
    }
}

fn expr_ok(e: &Expr) -> bool {
    match e {
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Ident(_) => true,
        Expr::Unary { expr, .. } => expr_ok(expr),
        Expr::Binary { lhs, rhs, .. } => expr_ok(lhs) && expr_ok(rhs),

        Expr::Call { callee, args } => {
            matches!(&**callee, Expr::Ident(_)) && args.iter().all(expr_ok)
        }
        Expr::Range { lo, hi } => expr_ok(lo) && expr_ok(hi),
        Expr::IfElse { cond, then, els } => {
            expr_ok(cond) && expr_ok(then) && expr_ok(els)
        }

        _ => false,
    }
}

pub type Vreg = u32;

pub type BlockId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Val {

    IntConst(i64),

    FloatConst(u64),

    Vreg(Vreg),

    Param(u32),
}

impl Val {

    pub fn float(x: f64) -> Val {
        Val::FloatConst(x.to_bits())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinKind {

    IAdd,
    ISub,
    IMul,

    DivConst(i64),

    ModConst(i64),

    IDiv,

    IMod,
    FAdd,
    FSub,
    FMul,
    FDiv,
    IEq,
    INe,
    ILt,
    ILe,
    IGt,
    IGe,
    FEq,
    FNe,
    FLt,
    FLe,
    FGt,
    FGe,
}

impl BinKind {

    pub fn is_cmp(self) -> bool {
        use BinKind::*;
        matches!(
            self,
            IEq | INe | ILt | ILe | IGt | IGe | FEq | FNe | FLt | FLe | FGt | FGe
        )
    }

    pub fn is_float(self) -> bool {
        use BinKind::*;
        matches!(
            self,
            FAdd | FSub | FMul | FDiv | FEq | FNe | FLt | FLe | FGt | FGe
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnKind {
    INeg,
    FNeg,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inst {

    Bin {
        dst: Vreg,
        op: BinKind,
        a: Val,
        b: Val,
        wrap: bool,
    },

    Un {
        dst: Vreg,
        op: UnKind,
        a: Val,
        wrap: bool,
    },

    Move { dst: Vreg, src: Val },

    Call {
        dst: Vreg,
        callee: String,
        args: Vec<Val>,
        is_float: bool,
    },

    Ret(Option<Val>),

    Br { cond: Val, t: BlockId, f: BlockId },

    Jmp(BlockId),

    Phi {
        dst: Vreg,
        srcs: Vec<(BlockId, Val)>,
    },
}

impl Inst {

    pub fn def(&self) -> Option<Vreg> {
        match self {
            Inst::Bin { dst, .. }
            | Inst::Un { dst, .. }
            | Inst::Move { dst, .. }
            | Inst::Call { dst, .. }
            | Inst::Phi { dst, .. } => Some(*dst),
            Inst::Ret(_) | Inst::Br { .. } | Inst::Jmp(_) => None,
        }
    }

    pub fn is_terminator(&self) -> bool {
        matches!(self, Inst::Ret(_) | Inst::Br { .. } | Inst::Jmp(_))
    }

    pub fn uses(&self) -> Vec<Vreg> {
        let mut v = Vec::new();
        let mut push = |val: &Val| {
            if let Val::Vreg(r) = val {
                v.push(*r);
            }
        };
        match self {
            Inst::Bin { a, b, .. } => {
                push(a);
                push(b);
            }
            Inst::Un { a, .. } => push(a),
            Inst::Move { src, .. } => push(src),
            Inst::Call { args, .. } => args.iter().for_each(&mut push),
            Inst::Ret(Some(val)) => push(val),
            Inst::Br { cond, .. } => push(cond),
            Inst::Ret(None) | Inst::Jmp(_) | Inst::Phi { .. } => {}
        }
        v
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub id: BlockId,
    pub insts: Vec<Inst>,
}

impl Block {
    pub fn new(id: BlockId) -> Block {
        Block {
            id,
            insts: Vec::new(),
        }
    }

    pub fn terminator(&self) -> Option<&Inst> {
        self.insts.last().filter(|i| i.is_terminator())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirFn {
    pub name: String,
    pub n_params: u32,

    pub param_is_float: Vec<bool>,
    pub ret_is_float: bool,
    pub entry: BlockId,
    pub blocks: Vec<Block>,
    pub next_vreg: Vreg,

    pub div_lines: HashMap<Vreg, u32>,
}

impl MirFn {
    pub fn new(name: impl Into<String>, param_is_float: Vec<bool>, ret_is_float: bool) -> MirFn {
        let n_params = param_is_float.len() as u32;
        MirFn {
            name: name.into(),
            n_params,
            param_is_float,
            ret_is_float,
            entry: 0,
            blocks: Vec::new(),
            next_vreg: 0,
            div_lines: std::collections::HashMap::new(),
        }
    }

    pub fn fresh_vreg(&mut self) -> Vreg {
        let v = self.next_vreg;
        self.next_vreg += 1;
        v
    }

    pub fn new_block(&mut self) -> BlockId {
        let id = self.blocks.len() as BlockId;
        self.blocks.push(Block::new(id));
        id
    }

    pub fn block_mut(&mut self, id: BlockId) -> &mut Block {
        &mut self.blocks[id as usize]
    }

    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id as usize]
    }

    // Structural SSA invariants the backends rely on: every block ends in
    // exactly one terminator, phis come first, and all branch targets exist.
    pub fn validate(&self) -> Result<(), String> {
        if self.blocks.is_empty() {
            return Err("function has no blocks".into());
        }
        if self.entry as usize >= self.blocks.len() {
            return Err("entry block out of range".into());
        }
        let n = self.blocks.len() as BlockId;
        for b in &self.blocks {
            match b.insts.last() {
                Some(i) if i.is_terminator() => {}
                _ => return Err(format!("block {} does not end in a terminator", b.id)),
            }

            let term_count = b.insts.iter().filter(|i| i.is_terminator()).count();
            if term_count != 1 {
                return Err(format!(
                    "block {} has {} terminators (want 1)",
                    b.id, term_count
                ));
            }

            let mut seen_nonphi = false;
            for i in &b.insts {
                match i {
                    Inst::Phi { .. } => {
                        if seen_nonphi {
                            return Err(format!("block {} has a phi after a non-phi", b.id));
                        }
                    }
                    _ => seen_nonphi = true,
                }
            }

            for i in &b.insts {
                match i {
                    Inst::Br { t, f, .. } if *t >= n || *f >= n => {
                        return Err(format!("block {} branches out of range", b.id));
                    }
                    Inst::Jmp(t) if *t >= n => {
                        return Err(format!("block {} jumps out of range", b.id));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

use crate::ast::{BinOp, UnOp};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dom {
    Int,
    Float,
}

impl Dom {
    fn is_float(self) -> bool {
        self == Dom::Float
    }
}

#[derive(Default)]
pub struct SigMap {
    pub ret_is_float: HashMap<String, bool>,
}

impl SigMap {

    pub fn from_program(prog: &crate::ast::Program) -> SigMap {
        let mut m = SigMap::default();
        for it in prog {
            if let crate::ast::Item::Fn(f) = it {
                if !f.is_method {
                    if let crate::ast::Type::Named(n) = &f.ret {
                        if n == "f64" {
                            m.ret_is_float.insert(f.name.clone(), true);
                        } else if n == "i64" {
                            m.ret_is_float.insert(f.name.clone(), false);
                        }
                    }
                }
            }
        }
        m
    }
}

struct Lowerer<'a> {
    f: MirFn,
    sigs: &'a SigMap,

    current_def: HashMap<String, HashMap<BlockId, Val>>,

    var_dom: HashMap<String, Dom>,

    preds: HashMap<BlockId, Vec<BlockId>>,

    sealed: std::collections::HashSet<BlockId>,

    incomplete_phis: HashMap<BlockId, Vec<(String, Vreg)>>,

    loops: Vec<(BlockId, BlockId)>,

    cur: Option<BlockId>,

    cur_line: u32,
}

impl<'a> Lowerer<'a> {
    fn new(name: &str, params: &[crate::ast::Param], ret_is_float: bool, sigs: &'a SigMap) -> Self {
        let param_is_float: Vec<bool> = params
            .iter()
            .map(|p| matches!(&p.ty, crate::ast::Type::Named(n) if n == "f64"))
            .collect();
        let mut f = MirFn::new(name, param_is_float.clone(), ret_is_float);
        let entry = f.new_block();
        f.entry = entry;
        let mut lw = Lowerer {
            f,
            sigs,
            current_def: HashMap::new(),
            var_dom: HashMap::new(),
            preds: HashMap::new(),
            sealed: std::collections::HashSet::new(),
            incomplete_phis: HashMap::new(),
            loops: Vec::new(),
            cur: Some(entry),
            cur_line: 0,
        };

        lw.sealed.insert(entry);

        for (i, p) in params.iter().enumerate() {
            let dom = if param_is_float[i] {
                Dom::Float
            } else {
                Dom::Int
            };
            lw.var_dom.insert(p.name.clone(), dom);
            lw.write_var(&p.name, entry, Val::Param(i as u32));
        }
        lw
    }

    fn write_var(&mut self, name: &str, block: BlockId, val: Val) {
        self.current_def
            .entry(name.to_string())
            .or_default()
            .insert(block, val);
    }

    fn read_var(&mut self, name: &str, block: BlockId) -> Val {
        if let Some(v) = self
            .current_def
            .get(name)
            .and_then(|m| m.get(&block))
            .copied()
        {
            return v;
        }
        self.read_rec(name, block)
    }

    // SSA name lookup that crosses block boundaries (Braun et al.). For an
    // unsealed block we cannot yet know all predecessors, so we park an
    // incomplete phi and fill its operands when the block is sealed.
    fn read_rec(&mut self, name: &str, block: BlockId) -> Val {
        let val = if !self.sealed.contains(&block) {
            let phi = self.fresh_phi(block);
            self.incomplete_phis
                .entry(block)
                .or_default()
                .push((name.to_string(), phi));
            Val::Vreg(phi)
        } else {
            let preds = self.preds.get(&block).cloned().unwrap_or_default();
            if preds.len() == 1 {

                self.read_var(name, preds[0])
            } else if preds.is_empty() {
                // Unreachable read (entry has no preds): pick a typed zero so
                // we never emit a dangling vreg.
                let dom = self.var_dom.get(name).copied().unwrap_or(Dom::Int);
                match dom {
                    Dom::Int => Val::IntConst(0),
                    Dom::Float => Val::float(0.0),
                }
            } else {

                let phi = self.fresh_phi(block);
                self.write_var(name, block, Val::Vreg(phi));
                self.add_phis(name, block, phi);
                self.drop_phi(block, phi)
            }
        };
        self.write_var(name, block, val);
        val
    }

    fn fresh_phi(&mut self, block: BlockId) -> Vreg {
        let dst = self.f.fresh_vreg();

        let pos = self
            .f
            .block(block)
            .insts
            .iter()
            .take_while(|i| matches!(i, Inst::Phi { .. }))
            .count();
        self.f
            .block_mut(block)
            .insts
            .insert(pos, Inst::Phi { dst, srcs: vec![] });
        dst
    }

    fn add_phis(&mut self, name: &str, block: BlockId, phi: Vreg) {
        let preds = self.preds.get(&block).cloned().unwrap_or_default();
        let mut srcs = Vec::with_capacity(preds.len());
        for p in &preds {
            let pv = self.read_var(name, *p);
            srcs.push((*p, pv));
        }
        self.set_phis(block, phi, srcs);
    }

    fn set_phis(&mut self, block: BlockId, phi: Vreg, srcs: Vec<(BlockId, Val)>) {
        for i in &mut self.f.block_mut(block).insts {
            if let Inst::Phi { dst, srcs: s } = i {
                if *dst == phi {
                    *s = srcs;
                    return;
                }
            }
        }
    }

    fn phi_srcs(&self, block: BlockId, phi: Vreg) -> Vec<(BlockId, Val)> {
        for i in &self.f.block(block).insts {
            if let Inst::Phi { dst, srcs } = i {
                if *dst == phi {
                    return srcs.clone();
                }
            }
        }
        vec![]
    }

    // Trivial-phi elimination: if a phi's only distinct operand (ignoring self
    // references) is a single value, the phi is redundant. Replace it everywhere
    // so SSA stays minimal and downstream uses see the real def.
    fn drop_phi(&mut self, block: BlockId, phi: Vreg) -> Val {
        let srcs = self.phi_srcs(block, phi);
        let mut same: Option<Val> = None;
        for (_, v) in &srcs {
            if *v == Val::Vreg(phi) {
                continue;
            }
            if let Some(s) = same {
                if s != *v {
                    // Two genuinely different operands: phi is real, keep it.
                    return Val::Vreg(phi);
                }
            } else {
                same = Some(*v);
            }
        }
        let replacement = same.unwrap_or(Val::Vreg(phi));

        self.f
            .block_mut(block)
            .insts
            .retain(|i| !matches!(i, Inst::Phi { dst, .. } if *dst == phi));
        self.replace_uses(Val::Vreg(phi), replacement);
        replacement
    }

    fn replace_uses(&mut self, old: Val, new: Val) {
        for b in &mut self.f.blocks {
            for i in &mut b.insts {
                match i {
                    Inst::Bin { a, b: bb, .. } => {
                        if *a == old {
                            *a = new;
                        }
                        if *bb == old {
                            *bb = new;
                        }
                    }
                    Inst::Un { a, .. } => {
                        if *a == old {
                            *a = new;
                        }
                    }
                    Inst::Move { src, .. } => {
                        if *src == old {
                            *src = new;
                        }
                    }
                    Inst::Call { args, .. } => {
                        for a in args {
                            if *a == old {
                                *a = new;
                            }
                        }
                    }
                    Inst::Ret(Some(v)) => {
                        if *v == old {
                            *v = new;
                        }
                    }
                    Inst::Br { cond, .. } => {
                        if *cond == old {
                            *cond = new;
                        }
                    }
                    Inst::Phi { srcs, .. } => {
                        for (_, v) in srcs {
                            if *v == old {
                                *v = new;
                            }
                        }
                    }
                    Inst::Ret(None) | Inst::Jmp(_) => {}
                }
            }
        }
        for m in self.current_def.values_mut() {
            for v in m.values_mut() {
                if *v == old {
                    *v = new;
                }
            }
        }
    }

    // Sealing a block declares all its predecessors known. Now we can fill in
    // any incomplete phis we parked earlier and collapse the trivial ones.
    fn seal_block(&mut self, block: BlockId) {
        if self.sealed.contains(&block) {
            return;
        }
        if let Some(phis) = self.incomplete_phis.remove(&block) {
            for (name, phi) in phis {
                self.add_phis(&name, block, phi);

                let v = self.drop_phi(block, phi);
                if v != Val::Vreg(phi) {
                    self.write_var(&name, block, v);
                }
            }
        }
        self.sealed.insert(block);
    }

    fn add_edge(&mut self, from: BlockId, to: BlockId) {
        self.preds.entry(to).or_default().push(from);
    }

    fn emit(&mut self, inst: Inst) {
        let b = self.cur.expect("emit into terminated block");
        self.f.block_mut(b).insts.push(inst);
    }

    fn emit_bin(&mut self, op: BinKind, a: Val, b: Val, wrap: bool) -> Val {
        let dst = self.f.fresh_vreg();
        self.emit(Inst::Bin {
            dst,
            op,
            a,
            b,
            wrap,
        });
        Val::Vreg(dst)
    }

    fn terminate(&mut self, term: Inst) {
        debug_assert!(term.is_terminator());
        self.emit(term);
        self.cur = None;
    }

    fn expr_dom(&self, e: &Expr) -> Dom {
        match e {
            Expr::Int(_) | Expr::Bool(_) => Dom::Int,
            Expr::Float(_) => Dom::Float,
            Expr::Ident(n) => self.var_dom.get(n).copied().unwrap_or(Dom::Int),
            Expr::Unary { op, expr } => match op {
                UnOp::Neg => self.expr_dom(expr),
                UnOp::Not => Dom::Int,
            },
            Expr::Binary { op, lhs, rhs } => match op {
                BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or
                | BinOp::In
                | BinOp::NotIn => Dom::Int,

                _ => {
                    if self.expr_dom(lhs).is_float() || self.expr_dom(rhs).is_float() {
                        Dom::Float
                    } else {
                        Dom::Int
                    }
                }
            },
            Expr::Call { callee, .. } => {
                if let Expr::Ident(n) = &**callee {
                    if self.sigs.ret_is_float.get(n).copied().unwrap_or(false) {
                        return Dom::Float;
                    }
                }
                Dom::Int
            }
            Expr::IfElse { then, els, .. } => {
                if self.expr_dom(then).is_float() || self.expr_dom(els).is_float() {
                    Dom::Float
                } else {
                    Dom::Int
                }
            }
            _ => Dom::Int,
        }
    }

    fn lower_expr(&mut self, e: &Expr) -> Val {
        match e {
            // Lumen ints are 48-bit; fold the literal into range at lowering time
            // so both backends see the same already-wrapped constant.
            Expr::Int(n) => Val::IntConst(crate::ast::wrap48(*n)),
            Expr::Float(x) => Val::float(*x),
            Expr::Bool(b) => Val::IntConst(if *b { 1 } else { 0 }),
            Expr::Ident(n) => {
                let b = self.cur.expect("expr in terminated block");
                self.read_var(n, b)
            }
            Expr::Unary { op, expr } => {
                let a = self.lower_expr(expr);
                match op {
                    UnOp::Neg => {
                        let dst = self.f.fresh_vreg();
                        let (k, wrap) = if self.expr_dom(expr).is_float() {
                            (UnKind::FNeg, false)
                        } else {
                            (UnKind::INeg, true)
                        };
                        self.emit(Inst::Un {
                            dst,
                            op: k,
                            a,
                            wrap,
                        });
                        Val::Vreg(dst)
                    }
                    UnOp::Not => {

                        self.emit_bin(BinKind::IEq, a, Val::IntConst(0), false)
                    }
                }
            }
            Expr::Binary { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs),
            Expr::Call { callee, args } => {
                let name = match &**callee {
                    Expr::Ident(n) => n.clone(),
                    _ => unreachable!("gate admits only direct-name calls"),
                };
                let arg_vals: Vec<Val> = args.iter().map(|a| self.lower_expr(a)).collect();
                let is_float = self.sigs.ret_is_float.get(&name).copied().unwrap_or(false);
                let dst = self.f.fresh_vreg();
                self.emit(Inst::Call {
                    dst,
                    callee: name,
                    args: arg_vals,
                    is_float,
                });
                Val::Vreg(dst)
            }
            Expr::IfElse { cond, then, els } => {

                let dom = self.expr_dom(e);
                let then_b = self.f.new_block();
                let else_b = self.f.new_block();
                let join_b = self.f.new_block();
                let cur = self.cur.expect("ifelse in terminated block");
                let cv = self.lower_cond(cond);
                self.add_edge(cur, then_b);
                self.add_edge(cur, else_b);
                self.terminate(Inst::Br {
                    cond: cv,
                    t: then_b,
                    f: else_b,
                });
                self.seal_block(then_b);
                self.seal_block(else_b);

                let tmp = format!("__ifx{}", self.f.next_vreg);
                self.var_dom.insert(tmp.clone(), dom);

                self.cur = Some(then_b);
                let tv = self.lower_expr(then);
                let then_end = self.cur.unwrap();
                self.write_var(&tmp, then_end, tv);
                self.add_edge(then_end, join_b);
                self.terminate(Inst::Jmp(join_b));

                self.cur = Some(else_b);
                let ev = self.lower_expr(els);
                let else_end = self.cur.unwrap();
                self.write_var(&tmp, else_end, ev);
                self.add_edge(else_end, join_b);
                self.terminate(Inst::Jmp(join_b));

                self.seal_block(join_b);
                self.cur = Some(join_b);
                self.read_var(&tmp, join_b)
            }
            _ => unreachable!("gate admits only numeric scalar expressions"),
        }
    }

    fn lower_binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Val {

        if matches!(op, BinOp::And | BinOp::Or) {
            return self.lower_shortcct(op, lhs, rhs);
        }
        let float = self.expr_dom(lhs).is_float() || self.expr_dom(rhs).is_float();
        let a = self.lower_expr(lhs);
        let b = self.lower_expr(rhs);
        let (kind, wrap) = self.bin_kind(op, float, rhs);
        let v = self.emit_bin(kind, a, b, wrap);

        if matches!(
            kind,
            BinKind::IDiv | BinKind::IMod | BinKind::DivConst(_) | BinKind::ModConst(_)
        ) {
            // Remember the source line of each integer divide so a div-by-zero
            // trap can report the right location at runtime.
            if let Val::Vreg(dst) = v {
                self.f.div_lines.insert(dst, self.cur_line);
            }
        }
        v
    }

    fn bin_kind(&self, op: BinOp, float: bool, rhs: &Expr) -> (BinKind, bool) {
        use BinKind::*;
        match (op, float) {
            // Only integer add/sub/mul wrap to 48 bits; float ops and comparisons never do.
            (BinOp::Add, false) => (IAdd, true),
            (BinOp::Sub, false) => (ISub, true),
            (BinOp::Mul, false) => (IMul, true),
            (BinOp::Add, true) => (FAdd, false),
            (BinOp::Sub, true) => (FSub, false),
            (BinOp::Mul, true) => (FMul, false),
            (BinOp::Div, true) => (FDiv, false),
            // Constant divisor: strength-reduce to a magic-number divide (DivConst);
            // fall back to a real IDiv only for a runtime or zero divisor.
            (BinOp::Div, false) => match const_int(rhs) {
                Some(c) if c != 0 => (DivConst(c), false),
                _ => (IDiv, false),
            },
            (BinOp::Mod, false) => match const_int(rhs) {
                Some(c) if c != 0 => (ModConst(c), false),
                _ => (IMod, false),
            },
            (BinOp::Eq, false) => (IEq, false),
            (BinOp::Ne, false) => (INe, false),
            (BinOp::Lt, false) => (ILt, false),
            (BinOp::Le, false) => (ILe, false),
            (BinOp::Gt, false) => (IGt, false),
            (BinOp::Ge, false) => (IGe, false),
            (BinOp::Eq, true) => (FEq, false),
            (BinOp::Ne, true) => (FNe, false),
            (BinOp::Lt, true) => (FLt, false),
            (BinOp::Le, true) => (FLe, false),
            (BinOp::Gt, true) => (FGt, false),
            (BinOp::Ge, true) => (FGe, false),

            _ => unreachable!("unsupported binary op reached MIR lowering: {:?}", op),
        }
    }

    fn lower_shortcct(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Val {
        let rhs_b = self.f.new_block();
        let join_b = self.f.new_block();
        let cur = self.cur.expect("sc in terminated block");
        let lv = self.lower_cond(lhs);
        let tmp = format!("__scx{}", self.f.next_vreg);
        self.var_dom.insert(tmp.clone(), Dom::Int);
        let (t_target, f_target) = (rhs_b, join_b);
        self.add_edge(cur, rhs_b);
        self.add_edge(cur, join_b);

        let sc_const = if matches!(op, BinOp::And) { 0 } else { 1 };

        let (br_t, br_f) = if matches!(op, BinOp::And) {
            (t_target, f_target)
        } else {
            (f_target, t_target)
        };
        self.write_var(&tmp, cur, Val::IntConst(sc_const));
        self.terminate(Inst::Br {
            cond: lv,
            t: br_t,
            f: br_f,
        });
        self.seal_block(rhs_b);

        self.cur = Some(rhs_b);
        let rv = self.lower_cond(rhs);
        let rhs_end = self.cur.unwrap();
        self.write_var(&tmp, rhs_end, rv);
        self.add_edge(rhs_end, join_b);
        self.terminate(Inst::Jmp(join_b));

        self.seal_block(join_b);
        self.cur = Some(join_b);
        self.read_var(&tmp, join_b)
    }

    fn lower_cond(&mut self, e: &Expr) -> Val {

        match e {
            Expr::Binary {
                op:
                    BinOp::Eq
                    | BinOp::Ne
                    | BinOp::Lt
                    | BinOp::Le
                    | BinOp::Gt
                    | BinOp::Ge
                    | BinOp::And
                    | BinOp::Or,
                ..
            } => self.lower_expr(e),
            _ => {
                let float = self.expr_dom(e).is_float();
                let v = self.lower_expr(e);
                let (k, zero) = if float {
                    (BinKind::FNe, Val::float(0.0))
                } else {
                    (BinKind::INe, Val::IntConst(0))
                };
                self.emit_bin(k, v, zero, false)
            }
        }
    }

    fn lower_block(&mut self, body: &[Stmt]) {
        for s in body {
            if self.cur.is_none() {
                break;
            }
            self.lower_stmt(s);
        }
    }

    fn lower_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::SrcLine(_) | Stmt::Break | Stmt::Continue => match s {
                Stmt::Break => {
                    if let Some(&(_, brk)) = self.loops.last() {
                        let cur = self.cur.unwrap();
                        self.add_edge(cur, brk);
                        self.terminate(Inst::Jmp(brk));
                    }
                }
                Stmt::Continue => {
                    if let Some(&(cont, _)) = self.loops.last() {
                        let cur = self.cur.unwrap();
                        self.add_edge(cur, cont);
                        self.terminate(Inst::Jmp(cont));
                    }
                }
                _ => {

                    if let Stmt::SrcLine(n) = s {
                        self.cur_line = *n;
                    }
                }
            },
            Stmt::Let { name, value, .. } => {
                let dom = self.expr_dom(value);
                let v = self.lower_expr(value);
                self.var_dom.insert(name.clone(), dom);
                let b = self.cur.unwrap();
                self.write_var(name, b, v);
            }
            Stmt::Assign { target, value } => {
                let name = match target {
                    Expr::Ident(n) => n.clone(),
                    _ => unreachable!("gate admits only ident assignment targets"),
                };
                let v = self.lower_expr(value);
                let b = self.cur.unwrap();
                self.write_var(&name, b, v);
            }
            Stmt::ExprStmt(e) => {
                let _ = self.lower_expr(e);
            }
            Stmt::Return(opt) => {
                let v = opt.as_ref().map(|e| self.lower_expr(e));
                self.terminate(Inst::Ret(v));
            }
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } => self.lower_if(cond, then, elifs, els),
            Stmt::While { cond, body } => self.lower_while(cond, body),
            Stmt::For { var, iter, body } => self.lower_range(var, iter, body),

            Stmt::Try { .. } | Stmt::Raise(_) => {
                unreachable!("try/raise are not MIR-eligible (see stmt_ok)")
            }
        }
    }

    fn lower_if(
        &mut self,
        cond: &Expr,
        then: &[Stmt],
        elifs: &[(Expr, Vec<Stmt>)],
        els: &Option<Vec<Stmt>>,
    ) {

        let then_b = self.f.new_block();
        let else_b = self.f.new_block();
        let join_b = self.f.new_block();
        let cur = self.cur.unwrap();
        let cv = self.lower_cond(cond);
        self.add_edge(cur, then_b);
        self.add_edge(cur, else_b);
        self.terminate(Inst::Br {
            cond: cv,
            t: then_b,
            f: else_b,
        });

        self.seal_block(then_b);
        self.seal_block(else_b);

        self.cur = Some(then_b);
        self.lower_block(then);
        if self.cur.is_some() {
            let b = self.cur.unwrap();
            self.add_edge(b, join_b);
            self.terminate(Inst::Jmp(join_b));
        }

        self.cur = Some(else_b);
        if elifs.is_empty() {
            if let Some(eb) = els {
                self.lower_block(eb);
            }
            if self.cur.is_some() {
                let b = self.cur.unwrap();
                self.add_edge(b, join_b);
                self.terminate(Inst::Jmp(join_b));
            }
        } else {

            let (c0, b0) = &elifs[0];
            let rest = &elifs[1..];
            self.lower_if(c0, b0, rest, els);
            if self.cur.is_some() {
                let b = self.cur.unwrap();
                self.add_edge(b, join_b);
                self.terminate(Inst::Jmp(join_b));
            }
        }

        self.seal_block(join_b);
        self.cur = Some(join_b);
    }

    fn lower_while(&mut self, cond: &Expr, body: &[Stmt]) {
        let header = self.f.new_block();
        let body_b = self.f.new_block();
        let exit_b = self.f.new_block();
        let cur = self.cur.unwrap();
        self.add_edge(cur, header);
        self.terminate(Inst::Jmp(header));

        self.cur = Some(header);
        let cv = self.lower_cond(cond);
        self.add_edge(header, body_b);
        self.add_edge(header, exit_b);
        self.terminate(Inst::Br {
            cond: cv,
            t: body_b,
            f: exit_b,
        });

        self.seal_block(body_b);

        self.loops.push((header, exit_b));
        self.cur = Some(body_b);
        self.lower_block(body);
        if self.cur.is_some() {
            let b = self.cur.unwrap();
            self.add_edge(b, header);
            self.terminate(Inst::Jmp(header));
        }
        self.loops.pop();

        self.seal_block(header);
        self.seal_block(exit_b);
        self.cur = Some(exit_b);
    }

    fn lower_range(&mut self, var: &str, iter: &Expr, body: &[Stmt]) {
        let (lo, hi) = match iter {
            Expr::Range { lo, hi } => (lo, hi),
            _ => unreachable!("gate admits only for-range"),
        };

        let lo_v = self.lower_expr(lo);
        let hi_v = self.lower_expr(hi);
        self.var_dom.insert(var.to_string(), Dom::Int);

        let limit = format!("__lim{}", self.f.next_vreg);
        self.var_dom.insert(limit.clone(), Dom::Int);

        let header = self.f.new_block();
        let body_b = self.f.new_block();
        let cont_b = self.f.new_block();
        let exit_b = self.f.new_block();

        let cur = self.cur.unwrap();
        self.write_var(var, cur, lo_v);
        self.write_var(&limit, cur, hi_v);
        self.add_edge(cur, header);
        self.terminate(Inst::Jmp(header));

        self.cur = Some(header);
        let vv = self.read_var(var, header);
        let lv = self.read_var(&limit, header);
        let cv = self.emit_bin(BinKind::ILt, vv, lv, false);
        self.add_edge(header, body_b);
        self.add_edge(header, exit_b);
        self.terminate(Inst::Br {
            cond: cv,
            t: body_b,
            f: exit_b,
        });

        self.seal_block(body_b);

        self.loops.push((cont_b, exit_b));
        self.cur = Some(body_b);
        self.lower_block(body);
        if self.cur.is_some() {
            let b = self.cur.unwrap();
            self.add_edge(b, cont_b);
            self.terminate(Inst::Jmp(cont_b));
        }
        self.loops.pop();

        self.seal_block(cont_b);

        self.cur = Some(cont_b);
        let cvv = self.read_var(var, cont_b);
        let nv = self.emit_bin(BinKind::IAdd, cvv, Val::IntConst(1), true);
        self.write_var(var, cont_b, nv);
        self.add_edge(cont_b, header);
        self.terminate(Inst::Jmp(header));

        self.seal_block(header);
        self.seal_block(exit_b);
        self.cur = Some(exit_b);
    }

    fn finish(mut self) -> MirFn {

        if let Some(b) = self.cur.take() {
            let v = if self.f.ret_is_float {
                Val::float(0.0)
            } else {
                Val::IntConst(0)
            };
            self.f.block_mut(b).insts.push(Inst::Ret(Some(v)));
        }
        self.f
    }
}

fn const_int(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(n) => Some(crate::ast::wrap48(*n)),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
        } => const_int(expr).map(|n| crate::ast::wrap48(-n)),
        _ => None,
    }
}

pub fn lower_fn(f: &FnDef, sigs: &SigMap) -> Result<MirFn, String> {
    let ret_is_float = matches!(&f.ret, crate::ast::Type::Named(n) if n == "f64");
    let mut lw = Lowerer::new(&f.name, &f.params, ret_is_float, sigs);
    lw.lower_block(&f.body);
    let mir = lw.finish();
    mir.validate()?;
    Ok(mir)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loc {

    Reg(&'static str),

    Spill,
}

#[derive(Debug, Clone, Default)]
pub struct RegAlloc {

    pub loc: std::collections::HashMap<Vreg, Loc>,

    pub callee_saved_used: Vec<&'static str>,
}

impl RegAlloc {
    pub fn reg_of(&self, v: Vreg) -> Option<&'static str> {
        match self.loc.get(&v) {
            Some(Loc::Reg(r)) => Some(*r),
            _ => None,
        }
    }
}

// Caller-saved (volatile) registers: free across calls, so prefer them for
// short-lived values that never span a call.
const MIR_VOLATILE: [&str; 4] = ["r8", "r9", "r10", "r11"];

// Callee-saved registers: survive calls but the prologue must push/pop any we
// touch. We reserve these for values whose live range crosses a call.
const MIR_CALLEE: [&str; 5] = ["rbx", "r12", "r13", "r14", "r15"];

#[derive(Clone, Copy, Debug)]
struct Interval {
    vreg: Vreg,
    start: usize,
    end: usize,
    across_call: bool,
}

// Linear-scan allocator over a single flattened instruction numbering.
pub fn regalloc(mir: &MirFn) -> RegAlloc {
    use std::collections::{HashMap, HashSet};

    let order: Vec<BlockId> = mir.blocks.iter().map(|b| b.id).collect();
    let mut blk_range: HashMap<BlockId, (usize, usize)> = HashMap::new();
    let mut gidx = 0usize;
    let mut call_indices: Vec<usize> = Vec::new();
    for &bid in &order {
        let b = mir.block(bid);
        let start = gidx;
        for inst in &b.insts {
            if matches!(inst, Inst::Call { .. }) {
                call_indices.push(gidx);
            }
            gidx += 1;
        }
        blk_range.insert(bid, (start, gidx));
    }
    let n_idx = gidx;
    if n_idx == 0 {
        return RegAlloc::default();
    }

    let mut live_in: HashMap<BlockId, HashSet<Vreg>> = HashMap::new();
    let mut live_out: HashMap<BlockId, HashSet<Vreg>> = HashMap::new();
    for &bid in &order {
        live_in.insert(bid, HashSet::new());
        live_out.insert(bid, HashSet::new());
    }
    let succs = |bid: BlockId| -> Vec<BlockId> {
        match mir.block(bid).insts.last() {
            Some(Inst::Br { t, f, .. }) => vec![*t, *f],
            Some(Inst::Jmp(t)) => vec![*t],
            _ => vec![],
        }
    };
    // Phi operands count as uses on the corresponding incoming edge, not in the
    // successor block, so record them per (pred, succ) edge for liveness below.
    let mut edge_uses: HashMap<(BlockId, BlockId), HashSet<Vreg>> = HashMap::new();
    for &bid in &order {
        for inst in &mir.block(bid).insts {
            if let Inst::Phi { srcs, .. } = inst {
                for (pred, val) in srcs {
                    if let Val::Vreg(r) = val {
                        edge_uses.entry((*pred, bid)).or_default().insert(*r);
                    }
                }
            }
        }
    }

    let mut changed = true;
    while changed {
        changed = false;

        for &bid in order.iter().rev() {

            let mut new_out: HashSet<Vreg> = HashSet::new();
            for s in succs(bid) {

                // A successor's phi defs are not live into the edge: the value
                // arrives via the phi operand (edge_uses), not as a live-through.
                let phi_defs: HashSet<Vreg> = mir
                    .block(s)
                    .insts
                    .iter()
                    .filter_map(|i| match i {
                        Inst::Phi { dst, .. } => Some(*dst),
                        _ => None,
                    })
                    .collect();
                for v in live_in[&s].iter() {
                    if !phi_defs.contains(v) {
                        new_out.insert(*v);
                    }
                }
                if let Some(e) = edge_uses.get(&(bid, s)) {
                    for v in e {
                        new_out.insert(*v);
                    }
                }
            }

            let mut live = new_out.clone();
            for inst in mir.block(bid).insts.iter().rev() {
                if let Some(d) = inst.def() {
                    live.remove(&d);
                }
                for u in inst.uses() {
                    live.insert(u);
                }

            }
            let new_in = live;
            if new_out != live_out[&bid] {
                live_out.insert(bid, new_out);
                changed = true;
            }
            if new_in != live_in[&bid] {
                live_in.insert(bid, new_in);
                changed = true;
            }
        }
    }

    let mut iv: HashMap<Vreg, (usize, usize)> = HashMap::new();
    let note = |iv: &mut HashMap<Vreg, (usize, usize)>, v: Vreg, idx: usize| {
        iv.entry(v)
            .and_modify(|(s, e)| {
                if idx < *s {
                    *s = idx;
                }
                if idx > *e {
                    *e = idx;
                }
            })
            .or_insert((idx, idx));
    };
    let mut gi = 0usize;
    for &bid in &order {
        for inst in &mir.block(bid).insts {
            if let Some(d) = inst.def() {
                note(&mut iv, d, gi);
            }
            for u in inst.uses() {
                note(&mut iv, u, gi);
            }
            gi += 1;
        }
    }

    for &bid in &order {
        let (bs, be) = blk_range[&bid];
        let last = be.saturating_sub(1);
        for v in live_in[&bid].iter() {
            note(&mut iv, *v, bs);
        }
        for v in live_out[&bid].iter() {
            note(&mut iv, *v, last);
        }
    }

    let mut intervals: Vec<Interval> = iv
        .into_iter()
        .map(|(vreg, (start, end))| {
            // A value whose range covers any call site must outlive it, so it
            // needs a callee-saved register (volatiles are clobbered by calls).
            let across_call = call_indices.iter().any(|&c| start <= c && c <= end);
            Interval {
                vreg,
                start,
                end,
                across_call,
            }
        })
        .collect();
    intervals.sort_by_key(|i| i.start);

    let mut ra = RegAlloc::default();
    let mut callee_used: HashSet<&'static str> = HashSet::new();

    struct Active {
        end: usize,
        reg: &'static str,
        callee: bool,
    }
    let mut active: Vec<Active> = Vec::new();
    let mut free_vol: Vec<&'static str> = MIR_VOLATILE.to_vec();
    let mut free_cal: Vec<&'static str> = MIR_CALLEE.to_vec();

    for itv in &intervals {

        active.retain(|a| {
            if a.end < itv.start {
                if a.callee {
                    free_cal.push(a.reg);
                } else {
                    free_vol.push(a.reg);
                }
                false
            } else {
                true
            }
        });

        let reg = if itv.across_call {
            // Must survive a call: callee-saved only.
            free_cal.pop()
        } else {
            // Free across calls: take a volatile first, fall back to callee-saved.
            free_vol.pop().or_else(|| free_cal.pop())
        };

        match reg {
            Some(r) => {
                let callee = MIR_CALLEE.contains(&r);
                if callee {
                    callee_used.insert(r);
                }
                ra.loc.insert(itv.vreg, Loc::Reg(r));
                active.push(Active {
                    end: itv.end,
                    reg: r,
                    callee,
                });
            }
            None => {
                ra.loc.insert(itv.vreg, Loc::Spill);
            }
        }
    }

    ra.callee_saved_used = MIR_CALLEE
        .iter()
        .copied()
        .filter(|r| callee_used.contains(r))
        .collect();
    ra
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Item;
    use crate::{lexer, parser};

    fn parse_fn(src: &str) -> FnDef {
        let toks = lexer::Lexer::new(src).tokenize().expect("lex");
        let prog = parser::Parser::new(toks).parse_program().expect("parse");
        prog.into_iter()
            .find_map(|it| match it {
                Item::Fn(f) => Some(f),
                _ => None,
            })
            .expect("a fn")
    }

    #[test]
    fn eligible_fib() {
        let f = parse_fn(
            "fn fib(n: i64) -> i64:\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\n",
        );
        assert!(mir_eligible(&f, &IntInfo::default()));
    }

    #[test]
    fn eligible_for_range_sum() {
        let f = parse_fn(
            "fn s(n: i64) -> i64:\n    let t = 0\n    for i in 0..n:\n        t = t + i\n    return t\n",
        );
        assert!(mir_eligible(&f, &IntInfo::default()));
    }

    #[test]
    fn inelig_uses_list() {
        let f = parse_fn("fn f(n: i64) -> i64:\n    let xs = [1, 2, 3]\n    return n\n");
        assert!(!mir_eligible(&f, &IntInfo::default()));
    }

    #[test]
    fn inelig_non_numeric() {
        let f = parse_fn("fn f(s: str) -> i64:\n    return 1\n");
        assert!(!mir_eligible(&f, &IntInfo::default()));
    }

    #[test]
    fn inelig_for_over_list() {
        let f = parse_fn(
            "fn f(n: i64) -> i64:\n    let t = 0\n    for x in [1, 2]:\n        t = t + x\n    return t\n",
        );
        assert!(!mir_eligible(&f, &IntInfo::default()));
    }

    #[test]
    fn val_float_roundtrip() {

        assert_eq!(Val::float(1.5), Val::FloatConst(1.5f64.to_bits()));
        assert_eq!(Val::float(-0.0), Val::FloatConst(0x8000_0000_0000_0000));

        assert_ne!(Val::float(-0.0), Val::float(0.0));
    }

    #[test]
    fn binkind_classification() {
        assert!(BinKind::ILt.is_cmp() && !BinKind::IAdd.is_cmp());
        assert!(BinKind::FLt.is_cmp() && BinKind::FLt.is_float());
        assert!(BinKind::FAdd.is_float() && !BinKind::IAdd.is_float());

        assert!(!BinKind::DivConst(7).is_cmp() && !BinKind::DivConst(7).is_float());
    }

    #[test]
    fn inst_def_and_terminator() {
        let bin = Inst::Bin {
            dst: 3,
            op: BinKind::IAdd,
            a: Val::Param(0),
            b: Val::IntConst(1),
            wrap: true,
        };
        assert_eq!(bin.def(), Some(3));
        assert!(!bin.is_terminator());

        let ret = Inst::Ret(Some(Val::Vreg(3)));
        assert_eq!(ret.def(), None);
        assert!(ret.is_terminator());

        assert!(Inst::Jmp(0).is_terminator());
        assert!(Inst::Br {
            cond: Val::Vreg(0),
            t: 1,
            f: 2
        }
        .is_terminator());
    }

    #[test]
    fn build_add_by_hand() {
        let mut f = MirFn::new("add", vec![false, false], false);
        let entry = f.new_block();
        assert_eq!(entry, 0);
        assert_eq!(f.entry, 0);
        let v = f.fresh_vreg();
        f.block_mut(entry).insts.push(Inst::Bin {
            dst: v,
            op: BinKind::IAdd,
            a: Val::Param(0),
            b: Val::Param(1),
            wrap: true,
        });
        f.block_mut(entry).insts.push(Inst::Ret(Some(Val::Vreg(v))));

        assert_eq!(f.n_params, 2);
        assert_eq!(f.blocks.len(), 1);
        assert_eq!(f.block(entry).insts.len(), 2);
        assert!(matches!(f.block(entry).terminator(), Some(Inst::Ret(_))));
        assert_eq!(f.validate(), Ok(()));

        let dbg = format!("{:?}", f.block(entry));
        assert!(dbg.contains("IAdd"));
        assert!(dbg.contains("Ret"));
    }

    #[test]
    fn build_loop_cfg() {

        let mut f = MirFn::new("loopy", vec![false], false);
        let entry = f.new_block();
        let header = f.new_block();
        let body = f.new_block();
        let exit = f.new_block();

        f.block_mut(entry).insts.push(Inst::Jmp(header));

        let iphi = f.fresh_vreg();
        let cmp = f.fresh_vreg();
        f.block_mut(header).insts.push(Inst::Phi {
            dst: iphi,
            srcs: vec![(entry, Val::IntConst(0)), (body, Val::Vreg(99))],
        });
        f.block_mut(header).insts.push(Inst::Bin {
            dst: cmp,
            op: BinKind::ILt,
            a: Val::Vreg(iphi),
            b: Val::Param(0),
            wrap: false,
        });
        f.block_mut(header).insts.push(Inst::Br {
            cond: Val::Vreg(cmp),
            t: body,
            f: exit,
        });

        let inext = f.fresh_vreg();
        f.block_mut(body).insts.push(Inst::Bin {
            dst: inext,
            op: BinKind::IAdd,
            a: Val::Vreg(iphi),
            b: Val::IntConst(1),
            wrap: true,
        });
        f.block_mut(body).insts.push(Inst::Jmp(header));

        f.block_mut(exit)
            .insts
            .push(Inst::Ret(Some(Val::IntConst(0))));

        assert_eq!(f.blocks.len(), 4);
        assert_eq!(f.validate(), Ok(()));
    }

    #[test]
    fn rejects_open_block() {
        let mut f = MirFn::new("bad", vec![], false);
        let b = f.new_block();
        f.block_mut(b).insts.push(Inst::Move {
            dst: 0,
            src: Val::IntConst(1),
        });
        assert!(f.validate().is_err());
    }

    #[test]
    fn rejects_phi_after_nonphi() {
        let mut f = MirFn::new("bad2", vec![], false);
        let b = f.new_block();
        f.block_mut(b).insts.push(Inst::Move {
            dst: 0,
            src: Val::IntConst(1),
        });
        f.block_mut(b).insts.push(Inst::Phi {
            dst: 1,
            srcs: vec![],
        });
        f.block_mut(b).insts.push(Inst::Ret(None));
        assert!(f.validate().is_err());
    }

    #[test]
    fn rejects_oob_branch() {
        let mut f = MirFn::new("bad3", vec![], false);
        let b = f.new_block();
        f.block_mut(b).insts.push(Inst::Jmp(99));
        assert!(f.validate().is_err());
    }

    fn lower(src: &str) -> MirFn {
        let toks = lexer::Lexer::new(src).tokenize().expect("lex");
        let prog = parser::Parser::new(toks).parse_program().expect("parse");
        let sigs = SigMap::from_program(&prog);
        let f = prog
            .iter()
            .find_map(|it| match it {
                Item::Fn(f) => Some(f),
                _ => None,
            })
            .expect("a fn");
        lower_fn(f, &sigs).expect("lowering succeeds")
    }

    fn count_phis(m: &MirFn) -> usize {
        m.blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter(|i| matches!(i, Inst::Phi { .. }))
            .count()
    }

    #[test]
    fn lower_add() {

        let m = lower("fn add(a: i64, b: i64) -> i64:\n    return a + b\n");
        assert!(m.validate().is_ok());
        assert_eq!(m.n_params, 2);
        assert!(!m.ret_is_float);
        assert_eq!(count_phis(&m), 0);

        let adds: Vec<_> = m
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter(|i| {
                matches!(
                    i,
                    Inst::Bin {
                        op: BinKind::IAdd,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(adds.len(), 1);
        if let Inst::Bin { a, b, wrap, .. } = adds[0] {
            assert_eq!(*a, Val::Param(0));
            assert_eq!(*b, Val::Param(1));
            assert!(*wrap, "integer add must carry the 48-bit wrap marker");
        }
    }

    #[test]
    fn lower_float_add() {
        let m = lower("fn fadd(a: f64, b: f64) -> f64:\n    return a + b\n");
        assert!(m.ret_is_float);
        let has_fadd = m.blocks.iter().flat_map(|b| &b.insts).any(|i| {
            matches!(
                i,
                Inst::Bin {
                    op: BinKind::FAdd,
                    wrap: false,
                    ..
                }
            )
        });
        assert!(has_fadd, "float add must use FAdd with no wrap");
    }

    #[test]
    fn lower_fib() {
        let m = lower(
            "fn fib(n: i64) -> i64:\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\n",
        );
        assert!(m.validate().is_ok());

        let has_br = m
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .any(|i| matches!(i, Inst::Br { .. }));
        assert!(has_br);

        let calls: Vec<_> = m
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter(|i| matches!(i, Inst::Call { callee, .. } if callee == "fib"))
            .collect();
        assert_eq!(calls.len(), 2);

        let rets = m
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter(|i| matches!(i, Inst::Ret(_)))
            .count();
        assert!(rets >= 2);
    }

    #[test]
    fn lower_for_range_phi() {

        let m = lower(
            "fn s(n: i64) -> i64:\n    let t = 0\n    for i in 0..n:\n        t = t + i\n    return t\n",
        );
        assert!(m.validate().is_ok());

        assert!(
            count_phis(&m) >= 2,
            "expected loop-carried phis for counter + accumulator, got {}",
            count_phis(&m)
        );

        let wrapped_adds = m
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter(|i| {
                matches!(
                    i,
                    Inst::Bin {
                        op: BinKind::IAdd,
                        wrap: true,
                        ..
                    }
                )
            })
            .count();
        assert!(wrapped_adds >= 2);
    }

    #[test]
    fn lower_while_ok() {
        let m = lower(
            "fn countdown(n: i64) -> i64:\n    let acc = 0\n    while n > 0:\n        acc = acc + n\n        n = n - 1\n    return acc\n",
        );
        assert!(m.validate().is_ok());
        assert!(count_phis(&m) >= 1, "loop-carried vars need phis");
    }

    #[test]
    fn lower_const_div_magic() {

        let m = lower("fn r(x: i64) -> i64:\n    return x % 7\n");
        let has_modconst = m.blocks.iter().flat_map(|b| &b.insts).any(|i| {
            matches!(
                i,
                Inst::Bin {
                    op: BinKind::ModConst(7),
                    ..
                }
            )
        });
        assert!(has_modconst, "constant modulo must use ModConst");
    }

    #[test]
    fn lower_runtime_div_idiv() {

        let m = lower("fn d(x: i64, y: i64) -> i64:\n    return x / y\n");
        let has_idiv = m.blocks.iter().flat_map(|b| &b.insts).any(|i| {
            matches!(
                i,
                Inst::Bin {
                    op: BinKind::IDiv,
                    ..
                }
            )
        });
        assert!(has_idiv);
    }

    #[test]
    fn lower_ssa_single_def() {
        let srcs = [
            "fn add(a: i64, b: i64) -> i64:\n    return a + b\n",
            "fn fib(n: i64) -> i64:\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\n",
            "fn s(n: i64) -> i64:\n    let t = 0\n    for i in 0..n:\n        t = t + i\n    return t\n",
            "fn cd(n: i64) -> i64:\n    let acc = 0\n    while n > 0:\n        acc = acc + n\n        n = n - 1\n    return acc\n",
            "fn g(a: i64, b: i64) -> i64:\n    if a > 0 and b > 0:\n        return a + b\n    return 0\n",
            "fn nested(n: i64) -> i64:\n    let t = 0\n    for i in 0..n:\n        for j in 0..i:\n            t = t + j\n    return t\n",
        ];
        for src in srcs {
            let m = lower(src);
            assert!(m.validate().is_ok(), "validate failed for: {src}");
            let mut seen = std::collections::HashSet::new();
            for b in &m.blocks {
                for i in &b.insts {
                    if let Some(d) = i.def() {
                        assert!(seen.insert(d), "vreg v{d} defined more than once in: {src}");
                    }
                }
            }

            for b in &m.blocks {
                let pset: std::collections::HashSet<BlockId> = m
                    .blocks
                    .iter()
                    .filter(|p| {
                        p.insts.iter().any(|i| match i {
                            Inst::Br { t, f, .. } => *t == b.id || *f == b.id,
                            Inst::Jmp(t) => *t == b.id,
                            _ => false,
                        })
                    })
                    .map(|p| p.id)
                    .collect();
                for i in &b.insts {
                    if let Inst::Phi { srcs, .. } = i {
                        for (pb, _) in srcs {
                            assert!(
                                pset.contains(pb),
                                "phi in block {} names non-predecessor {} in: {src}",
                                b.id,
                                pb
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn regalloc_sound() {
        let srcs = [
            "fn add(a: i64, b: i64) -> i64:\n    return a + b\n",
            "fn fib(n: i64) -> i64:\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\n",
            "fn s(n: i64) -> i64:\n    let t = 0\n    for i in 0..n:\n        t = t + i\n    return t\n",
            "fn cd(n: i64) -> i64:\n    let acc = 0\n    while n > 0:\n        acc = acc + n\n        n = n - 1\n    return acc\n",
        ];
        for src in srcs {
            let m = lower(src);
            let ra = regalloc(&m);
            for b in &m.blocks {
                for i in &b.insts {
                    if let Some(d) = i.def() {
                        assert!(ra.loc.contains_key(&d), "vreg v{d} unassigned in: {src}");
                    }
                }
            }
            let assigned_callee: std::collections::HashSet<&str> = ra
                .loc
                .values()
                .filter_map(|l| match l {
                    Loc::Reg(r) if MIR_CALLEE.contains(r) => Some(*r),
                    _ => None,
                })
                .collect();
            let reported: std::collections::HashSet<&str> =
                ra.callee_saved_used.iter().copied().collect();
            assert_eq!(assigned_callee, reported, "callee set mismatch in: {src}");
        }
    }

    #[test]
    fn regalloc_fib_callee_saved() {
        let m = lower(
            "fn fib(n: i64) -> i64:\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\n",
        );
        let ra = regalloc(&m);
        assert!(
            !ra.callee_saved_used.is_empty(),
            "fib should park a cross-call value in a callee-saved register"
        );
    }

    #[test]
    fn regalloc_prefers_volatile() {
        let m = lower("fn add(a: i64, b: i64) -> i64:\n    return a + b\n");
        let ra = regalloc(&m);
        assert!(
            ra.callee_saved_used.is_empty(),
            "call-free fn should not need callee-saved registers"
        );
    }
}
