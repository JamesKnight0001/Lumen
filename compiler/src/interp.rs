//! Tree-walking interpreter for Lumen, and the semantic reference for the
//! language. Whatever this file does, the native backend must reproduce
//! exactly: integer wrap at 48 bits, capture ordering, strict truthiness,
//! and the tail-call trampoline all have to stay byte-identical across both.

use crate::ast::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

// A hashable projection of a scalar key. Mirrors the native runtime's val_hash
// (lumen_rt.c): a whole-valued finite float folds to its int form, so 1 and 1.0
// are the SAME key - matching `==` (which coerces int/float) and the native
// backend. Non-integral / non-finite floats hash by bits. NaN and non-scalars
// aren't indexable (NaN != NaN; lists/maps never compare equal), so they're
// excluded and fall back to a linear scan, which stays correct.
#[derive(PartialEq, Eq, Hash)]
enum MapKey {
    Int(i64),
    Float(u64), // raw bits of a finite, non-whole f64
    Str(Rc<String>),
    Bool(bool),
    Nil,
}

fn map_key(v: &Value) -> Option<MapKey> {
    match v {
        Value::Int(n) => Some(MapKey::Int(*n)),
        Value::Bool(b) => Some(MapKey::Bool(*b)),
        Value::Nil => Some(MapKey::Nil),
        Value::Str(s) => Some(MapKey::Str(s.clone())),
        Value::Float(x) => {
            if x.is_nan() {
                None // NaN never equals anything
            } else if x.is_finite() && x.floor() == *x && *x >= -9.2e18 && *x <= 9.2e18 {
                Some(MapKey::Int(*x as i64)) // whole float == the int (and -0.0 -> 0)
            } else {
                Some(MapKey::Float(x.to_bits()))
            }
        }
        _ => None, // lists/maps/etc: never compare equal, so never indexed
    }
}

// Insertion-ordered map: a Vec keeps deterministic iteration order (the
// byte-identical contract requires it), and an index maps hashable keys to their
// Vec position for O(1) get/set/has/remove. Unhashable keys (see map_key) just
// aren't in the index; ops fall back to a linear scan for them, staying correct.
#[derive(Default)]
pub struct LumenMap {
    entries: Vec<(Value, Value)>,
    index: HashMap<MapKey, usize>,
}

impl LumenMap {
    fn len(&self) -> usize {
        self.entries.len()
    }

    // Position of key in entries, via the index when hashable, else linear scan.
    fn pos(&self, key: &Value) -> Option<usize> {
        if let Some(k) = map_key(key) {
            return self.index.get(&k).copied();
        }
        self.entries.iter().position(|(ek, _)| values_eq(ek, key))
    }

    fn get(&self, key: &Value) -> Option<Value> {
        self.pos(key).map(|i| self.entries[i].1.clone())
    }

    fn has(&self, key: &Value) -> bool {
        self.pos(key).is_some()
    }

    // Insert or overwrite, preserving the original position on overwrite.
    fn set(&mut self, key: Value, val: Value) {
        if let Some(i) = self.pos(&key) {
            self.entries[i].1 = val;
            return;
        }
        let i = self.entries.len();
        if let Some(k) = map_key(&key) {
            self.index.insert(k, i);
        }
        self.entries.push((key, val));
    }

    // Remove a key, returning its value. Removal shifts later positions, so the
    // index is rebuilt - O(n), but remove is rare next to get/set.
    fn remove(&mut self, key: &Value) -> Option<Value> {
        let i = self.pos(key)?;
        let (_, v) = self.entries.remove(i);
        self.reindex();
        Some(v)
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (i, (k, _)) in self.entries.iter().enumerate() {
            if let Some(mk) = map_key(k) {
                self.index.insert(mk, i);
            }
        }
    }

    fn keys(&self) -> Vec<Value> {
        self.entries.iter().map(|(k, _)| k.clone()).collect()
    }

    fn values(&self) -> Vec<Value> {
        self.entries.iter().map(|(_, v)| v.clone()).collect()
    }

    // Ordered (k, v) pairs, for iteration / printing / json. Insertion order.
    pub fn pairs(&self) -> &[(Value, Value)] {
        &self.entries
    }
}

// Build a LumenMap from ordered pairs (later duplicate keys overwrite earlier,
// matching map-literal semantics). Public so builtins (json.parse) can build maps.
pub fn lumen_map(pairs: Vec<(Value, Value)>) -> Rc<RefCell<LumenMap>> {
    let mut m = LumenMap::default();
    for (k, v) in pairs {
        m.set(k, v);
    }
    Rc::new(RefCell::new(m))
}

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(Rc<String>),
    Bool(bool),
    Nil,
    List(Rc<RefCell<Vec<Value>>>),
    Map(Rc<RefCell<LumenMap>>),

    Func(Rc<FnDef>, Rc<Vec<Value>>),
    Struct {
        name: String,
        fields: Rc<RefCell<Vec<(String, Value)>>>,
    },

    CBuf(Rc<RefCell<Vec<u8>>>),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{}", n),
            Value::Float(x) => {

                if x.fract() == 0.0 && x.is_finite() {
                    write!(f, "{:.1}", x)
                } else {
                    write!(f, "{}", x)
                }
            }
            Value::Str(s) => write!(f, "{}", s),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Nil => write!(f, "nil"),
            Value::List(items) => {
                let items = items.borrow();
                let parts: Vec<String> = items.iter().map(|v| v.repr()).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            Value::Map(entries) => {
                let entries = entries.borrow();
                let parts: Vec<String> = entries
                    .pairs()
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.repr(), v.repr()))
                    .collect();
                write!(f, "{{{}}}", parts.join(", "))
            }
            Value::Func(def, _) => write!(f, "<fn {}>", def.name),
            Value::Struct { name, fields } => {
                let fields = fields.borrow();
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v.repr()))
                    .collect();
                write!(f, "{}({})", name, parts.join(", "))
            }
            Value::CBuf(b) => write!(f, "<cbuf {}>", b.borrow().len()),
        }
    }
}

impl Value {
    fn repr(&self) -> String {
        match self {
            Value::Str(s) => format!("\"{}\"", s),
            other => format!("{}", other),
        }
    }
    fn truthy(&self) -> Result<bool, String> {
        match self {
            Value::Bool(b) => Ok(*b),
            Value::Nil => Ok(false),
            // Strict truthiness: only bool and nil are valid conditions. Numbers,
            // strings, and collections are NOT coerced. The native backend rejects
            // them identically, so do not loosen this.
            _ => Err("condition must be a bool (Lumen has strict truthiness)".into()),
        }
    }
}

pub struct Interp {
    funcs: HashMap<String, Rc<FnDef>>,
    structs: HashMap<String, Rc<StructDef>>,
    methods: HashMap<(String, String), Rc<FnDef>>,
    externs: HashMap<String, (String, ExternFn)>,
    globals: HashMap<String, Value>,

    current_line: u32,

    // Names the function we are currently inside (and its arity) so a `return f(...)`
    // to that same function can be recognized as a self tail-call. None outside any
    // self-recursive frame, e.g. inside methods where we do not trampoline.
    tail_fn: Option<(String, usize)>,
}

enum Flow {
    Normal,
    Return(Value),

    // Signals the trampoline in `invoke` to loop with new args instead of recursing,
    // so deep self-recursion runs in constant stack. Native lowers tail calls the
    // same way; both must agree on which calls qualify.
    TailCall(Vec<Value>),
    Break,
    Continue,
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    pub fn new() -> Self {
        Interp {
            funcs: HashMap::new(),
            structs: HashMap::new(),
            methods: HashMap::new(),
            externs: HashMap::new(),
            globals: HashMap::new(),
            current_line: 0,
            tail_fn: None,
        }
    }

    pub fn current_line(&self) -> u32 {
        self.current_line
    }

    pub fn run(&mut self, prog: &Program) -> Result<(), String> {
        let mut top_stmts: Vec<Stmt> = Vec::new();

        for item in prog {
            match item {
                Item::Fn(f) => {
                    self.funcs.insert(f.name.clone(), Rc::new(f.clone()));
                }
                Item::Struct(s) => {

                    let entry = self.structs.entry(s.name.clone()).or_insert_with(|| {
                        Rc::new(StructDef {
                            name: s.name.clone(),
                            fields: Vec::new(),
                            methods: Vec::new(),
                        })
                    });
                    let mut merged = (**entry).clone();
                    if !s.fields.is_empty() {
                        merged.fields = s.fields.clone();
                    }
                    for m in &s.methods {
                        self.methods
                            .insert((s.name.clone(), m.name.clone()), Rc::new(m.clone()));
                    }
                    *entry = Rc::new(merged);
                }
                Item::ExternBlock(b) => {
                    for ef in &b.fns {
                        self.externs
                            .insert(ef.name.clone(), (b.lib.clone(), ef.clone()));
                    }
                }
                Item::Import(_) => {  }
                Item::Stmt(s) => top_stmts.push(s.clone()),
            }
        }

        let mut env: HashMap<String, Value> = HashMap::new();
        let mut called_main = false;
        for s in &top_stmts {
            if let Stmt::ExprStmt(Expr::Call { callee, .. }) = s {
                if let Expr::Ident(n) = &**callee {
                    if n == "main" {
                        called_main = true;
                    }
                }
            }
            self.exec_stmt(s, &mut env)?;
        }

        if !called_main && self.funcs.contains_key("main") {
            self.call_function("main", vec![])?;
        }
        Ok(())
    }

    pub fn register_decls(&mut self, prog: &Program) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        for item in prog {
            match item {
                Item::Fn(f) => {
                    self.funcs.insert(f.name.clone(), Rc::new(f.clone()));
                }
                Item::Struct(s) => {
                    let entry = self.structs.entry(s.name.clone()).or_insert_with(|| {
                        Rc::new(StructDef {
                            name: s.name.clone(),
                            fields: Vec::new(),
                            methods: Vec::new(),
                        })
                    });
                    let mut merged = (**entry).clone();
                    if !s.fields.is_empty() {
                        merged.fields = s.fields.clone();
                    }
                    for m in &s.methods {
                        self.methods
                            .insert((s.name.clone(), m.name.clone()), Rc::new(m.clone()));
                    }
                    *entry = Rc::new(merged);
                }
                Item::ExternBlock(b) => {
                    for ef in &b.fns {
                        self.externs
                            .insert(ef.name.clone(), (b.lib.clone(), ef.clone()));
                    }
                }
                Item::Import(_) => {}
                Item::Stmt(s) => stmts.push(s.clone()),
            }
        }
        stmts
    }

    pub fn repl_exec(
        &mut self,
        s: &Stmt,
        env: &mut HashMap<String, Value>,
    ) -> Result<Option<Value>, String> {
        if let Stmt::ExprStmt(e) = s {
            let v = self.eval(e, env)?;
            return Ok(Some(v));
        }
        self.exec_stmt(s, env)?;
        Ok(None)
    }

    fn exec_block(
        &mut self,
        stmts: &[Stmt],
        env: &mut HashMap<String, Value>,
    ) -> Result<Flow, String> {
        for s in stmts {
            match self.exec_stmt(s, env)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_stmt(&mut self, s: &Stmt, env: &mut HashMap<String, Value>) -> Result<Flow, String> {
        match s {
            Stmt::Let { name, value, .. } => {
                let v = self.eval(value, env)?;
                env.insert(name.clone(), v);
                Ok(Flow::Normal)
            }
            Stmt::Assign { target, value } => {
                let v = self.eval(value, env)?;
                match target {
                    Expr::Ident(n) => {
                        if env.contains_key(n) {
                            env.insert(n.clone(), v);
                        } else {
                            self.globals.insert(n.clone(), v);
                        }
                    }
                    Expr::Field { obj, name } => {
                        let o = self.eval(obj, env)?;
                        if let Value::Struct { fields, .. } = o {
                            let mut fs = fields.borrow_mut();
                            if let Some(slot) = fs.iter_mut().find(|(k, _)| k == name) {
                                slot.1 = v;
                            } else {
                                fs.push((name.clone(), v));
                            }
                        } else {
                            return Err("field assignment on non-struct".into());
                        }
                    }
                    Expr::Index { obj, index } => {
                        let o = self.eval(obj, env)?;
                        let i = self.eval(index, env)?;
                        match (o, i) {
                            (Value::List(items), Value::Int(idx)) => {
                                let mut b = items.borrow_mut();
                                let idx = idx as usize;
                                if idx >= b.len() {
                                    return Err("index out of range".into());
                                }
                                b[idx] = v;
                            }
                            (Value::Map(entries), key) => {
                                entries.borrow_mut().set(key, v);
                            }
                            _ => return Err("bad index assignment".into()),
                        }
                    }
                    _ => return Err("invalid assignment target".into()),
                }
                Ok(Flow::Normal)
            }
            Stmt::ExprStmt(e) => {
                self.eval(e, env)?;
                Ok(Flow::Normal)
            }
            Stmt::Return(opt) => {

                // Detect `return f(args)` where f is the enclosing self-recursive
                // function with matching arity, and turn it into a TailCall the
                // trampoline can loop on. Args are evaluated in the current frame
                // BEFORE rebinding params, matching native's eval-then-jump order.
                if let Some(Expr::Call { callee, args }) = opt {
                    if let (Some((tname, tarity)), Expr::Ident(n)) = (&self.tail_fn, &**callee) {
                        if n == tname && args.len() == *tarity {

                            let tname = tname.clone();

                            let saved = self.tail_fn.take();
                            let mut vals = Vec::with_capacity(args.len());
                            for a in args {
                                vals.push(self.eval(a, env)?);
                            }
                            self.tail_fn = saved;
                            let _ = tname;
                            return Ok(Flow::TailCall(vals));
                        }
                    }
                }
                let v = match opt {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Nil,
                };
                Ok(Flow::Return(v))
            }
            Stmt::If {
                cond,
                then,
                elifs,
                els,
            } => {
                if self.eval(cond, env)?.truthy()? {
                    return self.exec_block(then, env);
                }
                for (c, body) in elifs {
                    if self.eval(c, env)?.truthy()? {
                        return self.exec_block(body, env);
                    }
                }
                if let Some(body) = els {
                    return self.exec_block(body, env);
                }
                Ok(Flow::Normal)
            }
            Stmt::While { cond, body } => {
                while self.eval(cond, env)?.truthy()? {
                    match self.exec_block(body, env)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal => {}
                        ret @ (Flow::Return(_) | Flow::TailCall(_)) => return Ok(ret),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::For { var, iter, body } => {
                let items = self.eval_iter(iter, env)?;
                for item in items {
                    env.insert(var.clone(), item);
                    match self.exec_block(body, env)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal => {}
                        ret @ (Flow::Return(_) | Flow::TailCall(_)) => return Ok(ret),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::Raise(e) => {

                let v = self.eval(e, env)?;
                Err(format!("{}", v))
            }
            Stmt::Try {
                body,
                catch_var,
                catch_body,
            } => {

                match self.exec_block(body, env) {
                    Ok(flow) => Ok(flow),
                    Err(msg) => {
                        env.insert(catch_var.clone(), Value::Str(Rc::new(msg)));
                        self.exec_block(catch_body, env)
                    }
                }
            }
            Stmt::SrcLine(n) => {
                self.current_line = *n;
                Ok(Flow::Normal)
            }
        }
    }

    fn eval_iter(
        &mut self,
        e: &Expr,
        env: &mut HashMap<String, Value>,
    ) -> Result<Vec<Value>, String> {
        match e {
            Expr::Range { lo, hi } => {
                let lo = self.eval(lo, env)?;
                let hi = self.eval(hi, env)?;
                if let (Value::Int(a), Value::Int(b)) = (lo, hi) {
                    Ok((a..b).map(Value::Int).collect())
                } else {
                    Err("range bounds must be ints".into())
                }
            }
            other => {
                let v = self.eval(other, env)?;
                match v {
                    Value::List(items) => Ok(items.borrow().clone()),

                    Value::Map(entries) => Ok(entries.borrow().keys()),

                    Value::Str(s) => Ok(s
                        .as_bytes()
                        .iter()
                        .map(|b| Value::Str(Rc::new((*b as char).to_string())))
                        .collect()),
                    _ => Err("for-loop target is not iterable".into()),
                }
            }
        }
    }

    fn eval(&mut self, e: &Expr, env: &mut HashMap<String, Value>) -> Result<Value, String> {
        match e {

            Expr::Lambda { .. } => Err("internal: unlifted lambda in interpreter".into()),
            Expr::Closure { fn_name, captures } => {
                let def = self
                    .funcs
                    .get(fn_name)
                    .cloned()
                    .ok_or_else(|| format!("internal: unknown closure fn {}", fn_name))?;
                let mut caps = Vec::with_capacity(captures.len());
                // Captures are evaluated and stored in source order, and `invoke_cap`
                // later prepends them to the args in this same order. Native lays out the
                // closure environment identically, so this ordering is load-bearing.
                for c in captures {
                    caps.push(self.eval(c, env)?);
                }
                Ok(Value::Func(def, Rc::new(caps)))
            }
            Expr::Int(n) => Ok(Value::Int(wrap48(*n))),
            Expr::Float(x) => Ok(Value::Float(*x)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Nil => Ok(Value::Nil),
            Expr::SelfExpr => env
                .get("self")
                .cloned()
                .ok_or_else(|| "self not in scope".into()),
            Expr::Ident(n) => env
                .get(n)
                .cloned()
                .or_else(|| self.globals.get(n).cloned())
                .or_else(|| {

                    self.funcs
                        .get(n)
                        .map(|f| Value::Func(f.clone(), Rc::new(Vec::new())))
                })
                .ok_or_else(|| format!("undefined variable: {}", n)),
            Expr::FStr(parts) => {
                let mut s = String::new();
                for p in parts {
                    match p {
                        FStrPart::Lit(l) => s.push_str(l),
                        FStrPart::Expr(e) => {
                            let v = self.eval(e, env)?;
                            s.push_str(&format!("{}", v));
                        }
                    }
                }
                Ok(Value::Str(Rc::new(s)))
            }
            Expr::List(elems) => {
                let mut vs = Vec::new();
                for el in elems {
                    vs.push(self.eval(el, env)?);
                }
                Ok(Value::List(Rc::new(RefCell::new(vs))))
            }
            Expr::Map(entries) => {
                let mut pairs: Vec<(Value, Value)> = Vec::new();
                for (k, v) in entries {
                    let kv = self.eval(k, env)?;
                    let vv = self.eval(v, env)?;
                    pairs.push((kv, vv));
                }
                Ok(Value::Map(lumen_map(pairs)))
            }
            Expr::Range { .. } => {
                let items = self.eval_iter(e, env)?;
                Ok(Value::List(Rc::new(RefCell::new(items))))
            }

            Expr::IfElse { cond, then, els } => {
                if self.eval(cond, env)?.truthy()? {
                    self.eval(then, env)
                } else {
                    self.eval(els, env)
                }
            }

            Expr::ListComp {
                elem,
                var,
                iter,
                cond,
            } => {
                let items = self.eval_iter(iter, env)?;
                let mut out = Vec::new();
                let saved = env.get(var).cloned();
                for item in items {
                    env.insert(var.clone(), item);
                    let keep = match cond {
                        Some(c) => self.eval(c, env)?.truthy()?,
                        None => true,
                    };
                    if keep {
                        out.push(self.eval(elem, env)?);
                    }
                }

                match saved {
                    Some(v) => {
                        env.insert(var.clone(), v);
                    }
                    None => {
                        env.remove(var);
                    }
                }
                Ok(Value::List(Rc::new(RefCell::new(out))))
            }
            Expr::Unary { op, expr } => {
                let v = self.eval(expr, env)?;
                match (op, v) {
                    (UnOp::Neg, Value::Int(n)) => Ok(Value::Int(wrap48(n.wrapping_neg()))),
                    (UnOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    _ => Err("bad operand to unary operator".into()),
                }
            }
            Expr::Binary { op, lhs, rhs } => {

                // `and`/`or` short-circuit: the rhs is not evaluated when the lhs
                // already decides the result. Both sides still go through strict
                // truthy(), so non-bool operands are an error rather than coerced.
                if *op == BinOp::And {
                    let l = self.eval(lhs, env)?;
                    if !l.truthy()? {
                        return Ok(Value::Bool(false));
                    }
                    return Ok(Value::Bool(self.eval(rhs, env)?.truthy()?));
                }
                if *op == BinOp::Or {
                    let l = self.eval(lhs, env)?;
                    if l.truthy()? {
                        return Ok(Value::Bool(true));
                    }
                    return Ok(Value::Bool(self.eval(rhs, env)?.truthy()?));
                }
                let l = self.eval(lhs, env)?;
                let r = self.eval(rhs, env)?;
                self.binop(*op, l, r)
            }
            Expr::Index { obj, index } => {
                let o = self.eval(obj, env)?;
                let i = self.eval(index, env)?;
                match (o, i) {
                    (Value::List(items), Value::Int(idx)) => {
                        let b = items.borrow();
                        let idx = idx as usize;
                        b.get(idx)
                            .cloned()
                            .ok_or_else(|| "index out of range".into())
                    }
                    (Value::Str(s), Value::Int(idx)) => s
                        .chars()
                        .nth(idx as usize)
                        .map(|c| Value::Str(Rc::new(c.to_string())))
                        .ok_or_else(|| "index out of range".into()),
                    (Value::Map(entries), key) => entries
                        .borrow()
                        .get(&key)
                        .ok_or_else(|| format!("key not found: {}", key.repr())),
                    _ => Err("bad index operation".into()),
                }
            }
            Expr::Slice { obj, lo, hi } => {
                let o = self.eval(obj, env)?;

                let lo = match lo {
                    None => None,
                    Some(e) => match self.eval(e, env)? {
                        Value::Int(n) => Some(n),
                        _ => return Err("slice bounds must be integers".into()),
                    },
                };
                let hi = match hi {
                    None => None,
                    Some(e) => match self.eval(e, env)? {
                        Value::Int(n) => Some(n),
                        _ => return Err("slice bounds must be integers".into()),
                    },
                };
                match o {
                    Value::List(items) => {
                        let b = items.borrow();
                        let len = b.len();
                        let (a, z) = slice_bounds(lo.unwrap_or(0), hi.unwrap_or(len as i64), len);
                        Ok(Value::List(Rc::new(RefCell::new(b[a..z].to_vec()))))
                    }

                    Value::Str(s) => {
                        let bytes = s.as_bytes();
                        let len = bytes.len();
                        let (a, z) = slice_bounds(lo.unwrap_or(0), hi.unwrap_or(len as i64), len);
                        let sub = String::from_utf8_lossy(&bytes[a..z]).into_owned();
                        Ok(Value::Str(Rc::new(sub)))
                    }
                    _ => Err("can only slice a list or string".into()),
                }
            }
            Expr::Field { obj, name } => {
                let o = self.eval(obj, env)?;
                match o {
                    Value::Struct { fields, .. } => fields
                        .borrow()
                        .iter()
                        .find(|(k, _)| k == name)
                        .map(|(_, v)| v.clone())
                        .ok_or_else(|| format!("no field '{}'", name)),
                    _ => Err(format!("cannot access field '{}' of non-struct", name)),
                }
            }
            Expr::Method { obj, name, args } => {

                if let Expr::Ident(modname) = &**obj {
                    if crate::builtins::is_module(modname) {
                        let mut a = Vec::new();
                        for arg in args {
                            a.push(self.eval(arg, env)?);
                        }
                        return match crate::builtins::lookup(modname, name) {
                            Some(f) => (f.eval)(&a),
                            None => Err(format!("{}: no function '{}'", modname, name)),
                        };
                    }
                }
                let recv = self.eval(obj, env)?;
                if let Value::List(ref items) = recv {
                    match name.as_str() {
                        "len" => return Ok(Value::Int(items.borrow().len() as i64)),
                        "push" => {
                            for a in args {
                                let v = self.eval(a, env)?;
                                items.borrow_mut().push(v);
                            }
                            return Ok(Value::Nil);
                        }
                        "reverse" => {
                            items.borrow_mut().reverse();
                            return Ok(Value::Nil);
                        }
                        "sort" => {
                            items.borrow_mut().sort_by(|a, b| {
                                num_of(a)
                                    .partial_cmp(&num_of(b))
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            });
                            return Ok(Value::Nil);
                        }
                        "pop" => {
                            return items
                                .borrow_mut()
                                .pop()
                                .ok_or_else(|| "pop from empty list".into());
                        }
                        "insert" => {
                            let idx = match self.eval(&args[0], env)? {
                                Value::Int(n) => n as usize,
                                _ => return Err("insert index must be int".into()),
                            };
                            let v = self.eval(&args[1], env)?;
                            let mut b = items.borrow_mut();
                            if idx > b.len() {
                                return Err("insert index out of range".into());
                            }
                            b.insert(idx, v);
                            return Ok(Value::Nil);
                        }
                        "contains" => {
                            let target = self.eval(&args[0], env)?;
                            let found = items.borrow().iter().any(|v| values_eq(v, &target));
                            return Ok(Value::Bool(found));
                        }
                        "index" => {
                            let target = self.eval(&args[0], env)?;
                            let idx = items
                                .borrow()
                                .iter()
                                .position(|v| values_eq(v, &target))
                                .map(|p| p as i64)
                                .unwrap_or(-1);
                            return Ok(Value::Int(idx));
                        }
                        "count" => {
                            let target = self.eval(&args[0], env)?;
                            let n = items
                                .borrow()
                                .iter()
                                .filter(|v| values_eq(v, &target))
                                .count() as i64;
                            return Ok(Value::Int(n));
                        }
                        "join" => {

                            let sep = match self.eval(&args[0], env)? {
                                Value::Str(s) => s.to_string(),
                                _ => return Err("join separator must be a string".into()),
                            };
                            let parts: Vec<String> =
                                items.borrow().iter().map(|v| format!("{}", v)).collect();
                            return Ok(Value::Str(Rc::new(parts.join(&sep))));
                        }
                        _ => {}
                    }
                }
                if let Value::Map(ref entries) = recv {
                    match name.as_str() {
                        "len" => return Ok(Value::Int(entries.borrow().len() as i64)),
                        "keys" => {
                            let ks = entries.borrow().keys();
                            return Ok(Value::List(Rc::new(RefCell::new(ks))));
                        }
                        "values" => {
                            let vs = entries.borrow().values();
                            return Ok(Value::List(Rc::new(RefCell::new(vs))));
                        }
                        "has" => {
                            let key = self.eval(&args[0], env)?;
                            return Ok(Value::Bool(entries.borrow().has(&key)));
                        }
                        "contains" => {
                            let key = self.eval(&args[0], env)?;
                            return Ok(Value::Bool(entries.borrow().has(&key)));
                        }
                        "get" => {
                            let key = self.eval(&args[0], env)?;
                            let hit = entries.borrow().get(&key);
                            return match hit {
                                Some(v) => Ok(v),
                                None => {
                                    if args.len() > 1 {
                                        self.eval(&args[1], env)
                                    } else {
                                        Ok(Value::Nil)
                                    }
                                }
                            };
                        }
                        "remove" => {
                            let key = self.eval(&args[0], env)?;
                            let removed = entries.borrow_mut().remove(&key);
                            return Ok(removed.unwrap_or(Value::Nil));
                        }
                        _ => {}
                    }
                }
                if let Value::Str(ref s) = recv {
                    match name.as_str() {
                        "len" => return Ok(Value::Int(s.chars().count() as i64)),
                        "upper" => return Ok(Value::Str(Rc::new(s.to_uppercase()))),
                        "lower" => return Ok(Value::Str(Rc::new(s.to_lowercase()))),
                        "trim" => {

                            let t =
                                s.trim_matches(|c| c == ' ' || c == '\t' || c == '\n' || c == '\r');
                            return Ok(Value::Str(Rc::new(t.to_string())));
                        }
                        "starts_with" => {
                            let pre = match self.eval(&args[0], env)? {
                                Value::Str(n) => n,
                                _ => return Err("starts_with arg must be a string".into()),
                            };
                            return Ok(Value::Bool(s.starts_with(&*pre)));
                        }
                        "ends_with" => {
                            let suf = match self.eval(&args[0], env)? {
                                Value::Str(n) => n,
                                _ => return Err("ends_with arg must be a string".into()),
                            };
                            return Ok(Value::Bool(s.ends_with(&*suf)));
                        }
                        "contains" => {
                            let needle = match self.eval(&args[0], env)? {
                                Value::Str(n) => n,
                                _ => return Err("contains arg must be a string".into()),
                            };
                            return Ok(Value::Bool(s.contains(&*needle)));
                        }
                        "split" => {
                            let sep = match self.eval(&args[0], env)? {
                                Value::Str(n) => n,
                                _ => return Err("split arg must be a string".into()),
                            };
                            let parts: Vec<Value> = if sep.is_empty() {
                                vec![Value::Str(Rc::new(s.to_string()))]
                            } else {
                                s.split(&*sep)
                                    .map(|p| Value::Str(Rc::new(p.to_string())))
                                    .collect()
                            };
                            return Ok(Value::List(Rc::new(RefCell::new(parts))));
                        }
                        "find" => {
                            let sub = match self.eval(&args[0], env)? {
                                Value::Str(n) => n,
                                _ => return Err("find arg must be a string".into()),
                            };

                            let idx = if sub.is_empty() {
                                0
                            } else {
                                s.find(&*sub).map(|i| i as i64).unwrap_or(-1)
                            };
                            return Ok(Value::Int(idx));
                        }
                        "replace" => {
                            let old = match self.eval(&args[0], env)? {
                                Value::Str(n) => n,
                                _ => return Err("replace arg must be a string".into()),
                            };
                            let new = match self.eval(&args[1], env)? {
                                Value::Str(n) => n,
                                _ => return Err("replace arg must be a string".into()),
                            };
                            let result = if old.is_empty() {
                                s.to_string()
                            } else {
                                s.replace(&*old, &new)
                            };
                            return Ok(Value::Str(Rc::new(result)));
                        }
                        "lstrip" => {
                            let t = s.trim_start_matches(|c| {
                                c == ' ' || c == '\t' || c == '\n' || c == '\r'
                            });
                            return Ok(Value::Str(Rc::new(t.to_string())));
                        }
                        "rstrip" => {
                            let t = s.trim_end_matches(|c| {
                                c == ' ' || c == '\t' || c == '\n' || c == '\r'
                            });
                            return Ok(Value::Str(Rc::new(t.to_string())));
                        }
                        "join" => {

                            if let Value::List(items) = self.eval(&args[0], env)? {
                                let parts: Vec<String> =
                                    items.borrow().iter().map(|v| format!("{}", v)).collect();
                                return Ok(Value::Str(Rc::new(parts.join(s))));
                            }
                            return Err("join arg must be a list".into());
                        }
                        "repeat" => {

                            let n = match self.eval(&args[0], env)? {
                                Value::Int(n) => n,
                                _ => return Err("repeat arg must be an int".into()),
                            };
                            let count = n.max(0) as usize;
                            let mut out = String::with_capacity(s.len() * count);
                            for _ in 0..count {
                                out.push_str(s);
                            }
                            return Ok(Value::Str(Rc::new(out)));
                        }
                        "title" => {

                            let bytes = s.as_bytes();
                            let mut out = String::with_capacity(s.len());
                            let mut at_wstart = true;
                            for &b in bytes {
                                let is_ws = b == b' ' || b == b'\t' || b == b'\n' || b == b'\r';
                                let c = if at_wstart {
                                    if b.is_ascii_lowercase() {
                                        b - 32
                                    } else {
                                        b
                                    }
                                } else if b.is_ascii_uppercase() {
                                    b + 32
                                } else {
                                    b
                                };
                                out.push(c as char);
                                at_wstart = is_ws;
                            }
                            return Ok(Value::Str(Rc::new(out)));
                        }
                        _ => {}
                    }
                }

                if let Value::Struct { name: sname, .. } = &recv {
                    if let Some(m) = self.methods.get(&(sname.clone(), name.clone())).cloned() {
                        let mut argvals = vec![recv.clone()];
                        for a in args {
                            argvals.push(self.eval(a, env)?);
                        }
                        return self.invoke(&m, argvals);
                    }
                }
                Err(format!("no method '{}' on value", name))
            }
            Expr::Call { callee, args } => {

                let name = match &**callee {
                    Expr::Ident(n) => n.clone(),
                    other => {
                        let callee_val = self.eval(other, env)?;
                        let mut argvals = Vec::new();
                        for a in args {
                            argvals.push(self.eval(a, env)?);
                        }
                        if let Value::Func(def, captured) = callee_val {
                            return self.invoke_cap(&def, argvals, &captured);
                        }
                        return Err("attempted to call a non-function value".into());
                    }
                };
                let mut argvals = Vec::new();
                for a in args {
                    argvals.push(self.eval(a, env)?);
                }

                match name.as_str() {
                    "print" => {
                        let parts: Vec<String> = argvals.iter().map(|v| format!("{}", v)).collect();
                        println!("{}", parts.join(" "));
                        return Ok(Value::Nil);
                    }
                    "len" => {
                        if let Some(Value::List(items)) = argvals.first() {
                            return Ok(Value::Int(items.borrow().len() as i64));
                        }
                        if let Some(Value::Map(entries)) = argvals.first() {
                            return Ok(Value::Int(entries.borrow().len() as i64));
                        }
                        if let Some(Value::Str(s)) = argvals.first() {
                            return Ok(Value::Int(s.chars().count() as i64));
                        }
                    }
                    "str" => {
                        return Ok(Value::Str(Rc::new(format!(
                            "{}",
                            argvals.first().cloned().unwrap_or(Value::Nil)
                        ))));
                    }
                    "int" => match argvals.first() {
                        Some(Value::Float(x)) => return Ok(Value::Int(*x as i64)),
                        Some(Value::Int(n)) => return Ok(Value::Int(*n)),
                        Some(Value::Str(s)) => {
                            return Ok(Value::Int(
                                s.trim().parse().map_err(|_| "int() parse error")?,
                            ))
                        }
                        _ => {}
                    },
                    "float" => match argvals.first() {
                        Some(Value::Int(n)) => return Ok(Value::Float(*n as f64)),
                        Some(Value::Float(x)) => return Ok(Value::Float(*x)),
                        Some(Value::Str(s)) => {
                            return Ok(Value::Float(
                                s.trim().parse().map_err(|_| "float() parse error")?,
                            ))
                        }
                        _ => {}
                    },

                    "ord" => {
                        if let Some(Value::Str(s)) = argvals.first() {
                            let b = s.bytes().next().unwrap_or(0);
                            return Ok(Value::Int(b as i64));
                        }
                    }

                    "chr" => {
                        if let Some(Value::Int(n)) = argvals.first() {
                            let c = (*n as u8) as char;
                            return Ok(Value::Str(Rc::new(c.to_string())));
                        }
                    }

                    "is_digit" => {
                        if let Some(Value::Str(s)) = argvals.first() {
                            let d = s
                                .bytes()
                                .next()
                                .map(|b| b.is_ascii_digit())
                                .unwrap_or(false);
                            return Ok(Value::Bool(d));
                        }
                    }

                    "is_alpha" => {
                        if let Some(Value::Str(s)) = argvals.first() {
                            let a = s
                                .bytes()
                                .next()
                                .map(|b| b.is_ascii_alphabetic())
                                .unwrap_or(false);
                            return Ok(Value::Bool(a));
                        }
                    }

                    "is_space" => {
                        if let Some(Value::Str(s)) = argvals.first() {
                            let w = s
                                .bytes()
                                .next()
                                .map(|b| b.is_ascii_whitespace())
                                .unwrap_or(false);
                            return Ok(Value::Bool(w));
                        }
                    }

                    "input" => {
                        use std::io::Write;
                        if let Some(p) = argvals.first() {
                            print!("{}", p);
                            let _ = std::io::stdout().flush();
                        }
                        let mut line = String::new();
                        let n = std::io::stdin().read_line(&mut line).unwrap_or(0);
                        if n == 0 {
                            return Ok(Value::Nil);
                        }
                        let trimmed = line.trim_end_matches(['\n', '\r']).to_string();
                        return Ok(Value::Str(Rc::new(trimmed)));
                    }
                    "abs" => match argvals.first() {
                        Some(Value::Int(n)) => return Ok(Value::Int(n.abs())),
                        Some(Value::Float(x)) => return Ok(Value::Float(x.abs())),
                        _ => {}
                    },
                    "round" => match argvals.first() {
                        Some(Value::Int(n)) => return Ok(Value::Int(*n)),
                        Some(Value::Float(x)) => return Ok(Value::Int(wrap48(x.round() as i64))),
                        _ => {}
                    },
                    "type" => {
                        let t = match argvals.first() {
                            Some(Value::Int(_)) => "i64",
                            Some(Value::Float(_)) => "f64",
                            Some(Value::Bool(_)) => "bool",
                            Some(Value::Str(_)) => "str",
                            Some(Value::List(_)) => "list",
                            Some(Value::Map(_)) => "map",
                            Some(Value::Func(..)) => "fn",
                            Some(Value::Struct { .. }) => "struct",
                            Some(Value::CBuf(_)) => "cbuf",
                            Some(Value::Nil) | None => "nil",
                        };
                        return Ok(Value::Str(Rc::new(t.to_string())));
                    }
                    "assert" => {
                        let ok = match argvals.first() {
                            Some(v) => v.truthy()?,
                            None => false,
                        };
                        if !ok {
                            return Err("assertion failed".into());
                        }
                        return Ok(Value::Nil);
                    }

                    "drop" => {
                        return Ok(Value::Nil);
                    }
                    "sum" | "min" | "max" => {
                        if let Some(Value::List(items)) = argvals.first() {
                            let items = items.borrow();
                            if name == "sum" {
                                let all_int = items.iter().all(|v| matches!(v, Value::Int(_)));
                                if all_int {
                                    let s: i64 = items
                                        .iter()
                                        .map(|v| if let Value::Int(n) = v { *n } else { 0 })
                                        .sum();
                                    return Ok(Value::Int(s));
                                }
                                let s: f64 = items.iter().map(num_of).sum();
                                return Ok(Value::Float(s));
                            }
                            if items.is_empty() {
                                return Err(format!("{}() of empty list", name));
                            }
                            let mut best = items[0].clone();
                            for v in items.iter().skip(1) {
                                let take = if name == "min" {
                                    num_of(v) < num_of(&best)
                                } else {
                                    num_of(v) > num_of(&best)
                                };
                                if take {
                                    best = v.clone();
                                }
                            }
                            return Ok(best);
                        }
                    }
                    "range" => {
                        let (lo, hi) = match (argvals.first(), argvals.get(1)) {
                            (Some(Value::Int(n)), None) => (0i64, *n),
                            (Some(Value::Int(a)), Some(Value::Int(b))) => (*a, *b),
                            _ => return Err("range() takes 1 or 2 ints".into()),
                        };
                        let v: Vec<Value> = (lo..hi).map(Value::Int).collect();
                        return Ok(Value::List(Rc::new(RefCell::new(v))));
                    }
                    _ => {}
                }

                if let Some(sd) = self.structs.get(&name).cloned() {
                    let mut fields_vec: Vec<(String, Value)> = Vec::new();
                    for (i, f) in sd.fields.iter().enumerate() {
                        fields_vec.push((
                            f.name.clone(),
                            argvals.get(i).cloned().unwrap_or(Value::Nil),
                        ));
                    }
                    return Ok(Value::Struct {
                        name: name.clone(),
                        fields: Rc::new(RefCell::new(fields_vec)),
                    });
                }

                if self.externs.contains_key(&name) {
                    return self.call_extern(&name, &argvals);
                }

                if let Some(f) = self.funcs.get(&name).cloned() {
                    return self.invoke(&f, argvals);
                }

                if let Some(Value::Func(def, captured)) = env
                    .get(&name)
                    .cloned()
                    .or_else(|| self.globals.get(&name).cloned())
                {
                    return self.invoke_cap(&def, argvals, &captured);
                }
                Err(format!("undefined function: {}", name))
            }
            Expr::NamedCall { callee, args } => {
                let name = match &**callee {
                    Expr::Ident(n) => n.clone(),
                    _ => return Err("named call requires a type name".into()),
                };
                let sd = self
                    .structs
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| format!("no struct named '{}'", name))?;
                let mut provided: Vec<(String, Value)> = Vec::new();
                for (fname, fexpr) in args {
                    let v = self.eval(fexpr, env)?;
                    provided.push((fname.clone(), v));
                }

                let mut fields_vec: Vec<(String, Value)> = Vec::new();
                for f in &sd.fields {
                    let v = provided
                        .iter()
                        .find(|(k, _)| k == &f.name)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Value::Nil);
                    fields_vec.push((f.name.clone(), v));
                }
                Ok(Value::Struct {
                    name: name.clone(),
                    fields: Rc::new(RefCell::new(fields_vec)),
                })
            }
        }
    }

    fn invoke(&mut self, f: &FnDef, args: Vec<Value>) -> Result<Value, String> {
        let mut local: HashMap<String, Value> = HashMap::new();
        for (p, a) in f.params.iter().zip(args) {
            local.insert(p.name.clone(), a);
        }

        // Arm self tail-calls for plain functions only. Methods are excluded
        // (take() clears tail_fn) because dispatch there is not a simple self-jump.
        let saved = if f.is_method {
            self.tail_fn.take()
        } else {
            self.tail_fn.replace((f.name.clone(), f.params.len()))
        };
        // Trampoline: a TailCall rebinds params in place and loops instead of
        // recursing, so self-recursion uses O(1) Rust stack. This is the behavior
        // the native backend's tail-call lowering must match exactly.
        let result = loop {
            match self.exec_block(&f.body, &mut local) {
                Ok(Flow::TailCall(vals)) => {
                    local.clear();
                    for (p, a) in f.params.iter().zip(vals) {
                        local.insert(p.name.clone(), a);
                    }

                }
                Ok(Flow::Return(v)) => break Ok(v),
                Ok(_) => break Ok(Value::Nil),
                Err(e) => break Err(e),
            }
        };
        self.tail_fn = saved;
        result
    }

    fn invoke_cap(
        &mut self,
        f: &FnDef,
        args: Vec<Value>,
        captured: &Rc<Vec<Value>>,
    ) -> Result<Value, String> {

        let mut local: HashMap<String, Value> = HashMap::new();
        // Captured values come first, then the call args, matching the order they
        // were collected in the Closure case above and how native lays out the frame.
        let full = captured.iter().cloned().chain(args);
        for (p, a) in f.params.iter().zip(full) {
            local.insert(p.name.clone(), a);
        }

        // No trampoline here: closures do not self tail-call, so clear tail_fn
        // for the duration so an inner `return f(...)` is not misread as one.
        let saved = self.tail_fn.take();
        let r = self.exec_block(&f.body, &mut local);
        self.tail_fn = saved;
        match r? {
            Flow::Return(v) => Ok(v),
            _ => Ok(Value::Nil),
        }
    }

    fn call_function(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        let f = self
            .funcs
            .get(name)
            .cloned()
            .ok_or_else(|| format!("no function {}", name))?;
        self.invoke(&f, args)
    }

    fn value_in(&self, x: &Value, container: &Value) -> Result<bool, String> {
        match container {
            Value::List(items) => Ok(items.borrow().iter().any(|e| values_eq(e, x))),
            Value::Map(entries) => Ok(entries.borrow().has(x)),
            Value::Str(hay) => match x {
                Value::Str(needle) => Ok(hay.contains(needle.as_str())),
                _ => Err("'in' on a string requires a string on the left".into()),
            },
            _ => Err("'in' requires a list, map, or string on the right".into()),
        }
    }

    fn binop(&self, op: BinOp, l: Value, r: Value) -> Result<Value, String> {
        use Value::*;
        Ok(match (op, l, r) {

            // Integer arithmetic wraps to 48 bits (wrap48 after a 64-bit wrapping op).
            // 48 bits is the NaN-box payload width, so every result must round-trip
            // through it identically on the native side. Never use checked/native +.
            (BinOp::Add, Int(a), Int(b)) => Int(wrap48(a.wrapping_add(b))),
            (BinOp::Sub, Int(a), Int(b)) => Int(wrap48(a.wrapping_sub(b))),
            (BinOp::Mul, Int(a), Int(b)) => Int(wrap48(a.wrapping_mul(b))),

            (BinOp::Pow, Int(a), Int(b)) => {
                if b >= 0 {
                    let mut acc: i64 = 1;
                    let mut i: i64 = 0;
                    while i < b {
                        acc = wrap48(acc.wrapping_mul(a));
                        i += 1;
                    }
                    Int(acc)
                } else {
                    Float((a as f64).powf(b as f64))
                }
            }
            (BinOp::Div, Int(a), Int(b)) => {
                if b == 0 {
                    return Err("division by zero".into());
                }
                Int(wrap48(a.wrapping_div(b)))
            }
            (BinOp::Mod, Int(a), Int(b)) => {
                if b == 0 {
                    return Err("modulo by zero".into());
                }
                Int(wrap48(a.wrapping_rem(b)))
            }
            (BinOp::Add, Float(a), Float(b)) => Float(a + b),
            (BinOp::Sub, Float(a), Float(b)) => Float(a - b),
            (BinOp::Mul, Float(a), Float(b)) => Float(a * b),
            (BinOp::Pow, Float(a), Float(b)) => Float(a.powf(b)),
            (BinOp::Div, Float(a), Float(b)) => Float(a / b),
            (BinOp::Mod, Float(a), Float(b)) => Float(a % b),

            (op, Int(a), Float(b)) => return self.binop(op, Float(a as f64), Float(b)),
            (op, Float(a), Int(b)) => return self.binop(op, Float(a), Float(b as f64)),
            (BinOp::Add, Str(a), Str(b)) => Str(Rc::new(format!("{}{}", a, b))),
            (BinOp::Eq, a, b) => Bool(values_eq(&a, &b)),
            (BinOp::Ne, a, b) => Bool(!values_eq(&a, &b)),

            (BinOp::In, x, c) => Bool(self.value_in(&x, &c)?),
            (BinOp::NotIn, x, c) => Bool(!self.value_in(&x, &c)?),
            (BinOp::Lt, Int(a), Int(b)) => Bool(a < b),
            (BinOp::Le, Int(a), Int(b)) => Bool(a <= b),
            (BinOp::Gt, Int(a), Int(b)) => Bool(a > b),
            (BinOp::Ge, Int(a), Int(b)) => Bool(a >= b),
            (BinOp::Lt, Float(a), Float(b)) => Bool(a < b),
            (BinOp::Le, Float(a), Float(b)) => Bool(a <= b),
            (BinOp::Gt, Float(a), Float(b)) => Bool(a > b),
            (BinOp::Ge, Float(a), Float(b)) => Bool(a >= b),

            (BinOp::Lt, Str(a), Str(b)) => Bool(a < b),
            (BinOp::Le, Str(a), Str(b)) => Bool(a <= b),
            (BinOp::Gt, Str(a), Str(b)) => Bool(a > b),
            (BinOp::Ge, Str(a), Str(b)) => Bool(a >= b),
            (op, _, _) => return Err(format!("unsupported operands for {:?}", op)),
        })
    }

    #[cfg(windows)]
    fn call_extern(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        crate::ffi::call_dll(self.externs.get(name).unwrap(), args)
    }
    #[cfg(not(windows))]
    fn call_extern(&mut self, _name: &str, _args: &[Value]) -> Result<Value, String> {
        Err("FFI only implemented on Windows in this build".into())
    }
}

fn slice_bounds(lo: i64, hi: i64, len: usize) -> (usize, usize) {
    let n = len as i64;
    let norm = |i: i64| -> i64 {
        let i = if i < 0 { i + n } else { i };
        i.clamp(0, n)
    };
    let a = norm(lo);
    let z = norm(hi);
    if a >= z {
        (a as usize, a as usize)
    } else {
        (a as usize, z as usize)
    }
}

fn values_eq(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (Int(x), Int(y)) => x == y,
        (Float(x), Float(y)) => x == y,
        (Str(x), Str(y)) => x == y,
        (Bool(x), Bool(y)) => x == y,
        (Nil, Nil) => true,
        _ => false,
    }
}

fn num_of(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Float(x) => *x,
        _ => 0.0,
    }
}
