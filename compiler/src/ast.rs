
//! Abstract syntax tree for Lumen. Mostly plain data: the node definitions
//! the parser builds and every later pass walks. `wrap48` lives here because
//! both backends need the exact same 48-bit integer wrapping.
#![allow(dead_code)]

#[derive(Debug, Clone)]
pub enum Type {
    Named(String),
    Dynamic,
    Nil,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    // Default value for an omitted trailing arg (e.g. `fn f(a, b=10)`).
    pub default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub enum Item {
    Fn(FnDef),
    Struct(StructDef),
    ExternBlock(ExternBlock),
    Import(ImportDef),
    Stmt(Stmt),
}

#[derive(Debug, Clone)]
pub struct FnDef {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Type,
    pub body: Vec<Stmt>,
    pub exported: bool,
    pub is_method: bool,
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<Field>,
    pub methods: Vec<FnDef>,
}

#[derive(Debug, Clone)]
pub struct ExternFn {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Type,
}

#[derive(Debug, Clone)]
pub struct ExternBlock {
    pub abi: String,
    pub lib: String,
    pub fns: Vec<ExternFn>,
}

#[derive(Debug, Clone)]
pub struct ImportDef {
    pub module: String,
    pub alias: Option<String>,
    pub names: Vec<String>,
    // Leading-dot count for relative imports: 0 = absolute (sibling/package),
    // 1 = `.mod` (same dir), 2 = `..mod` (parent), n = (n-1) dirs up.
    pub level: u8,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: String,
        mutable: bool,
        ty: Type,
        value: Expr,
    },
    Assign {
        target: Expr,
        value: Expr,
    },
    ExprStmt(Expr),
    Return(Option<Expr>),
    If {
        cond: Expr,
        then: Vec<Stmt>,
        elifs: Vec<(Expr, Vec<Stmt>)>,
        els: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        var: String,
        iter: Expr,
        body: Vec<Stmt>,
    },
    Break,
    Continue,

    Try {
        body: Vec<Stmt>,
        catch_var: String,
        catch_body: Vec<Stmt>,
    },

    Raise(Expr),

    SrcLine(u32),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(std::rc::Rc<String>),
    FStr(Vec<FStrPart>),
    Bool(bool),
    Nil,
    Ident(String),
    SelfExpr,
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    NamedCall {
        callee: Box<Expr>,
        args: Vec<(String, Expr)>,
    },
    Field {
        obj: Box<Expr>,
        name: String,
    },
    Method {
        obj: Box<Expr>,
        name: String,
        args: Vec<Expr>,
    },
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Range {
        lo: Box<Expr>,
        hi: Box<Expr>,
    },

    IfElse {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },

    ListComp {
        elem: Box<Expr>,
        var: String,
        iter: Box<Expr>,
        cond: Option<Box<Expr>>,
    },
    Index {
        obj: Box<Expr>,
        index: Box<Expr>,
    },

    Slice {
        obj: Box<Expr>,
        lo: Option<Box<Expr>>,
        hi: Option<Box<Expr>>,
    },

    Lambda {
        params: Vec<String>,
        body: Vec<Stmt>,
    },

    Closure {
        fn_name: String,
        captures: Vec<Expr>,
    },
}

#[derive(Debug, Clone)]
pub enum FStrPart {
    Lit(String),
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Pow,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    In,
    NotIn,
}

pub type Program = Vec<Item>;

// Kind of a named declaration, for span side-tables (tooling only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    Fn,
    Method,
    Struct,
    Field,
    Param,
    Import,
}

// Source span of a declaration NAME. Built only by `parse_program_spanned`
// (used by tooling/LSP); the compile path never produces these, so codegen
// and runtime are unaffected. Positions are 1-based; end col is exclusive.
#[derive(Debug, Clone)]
pub struct DeclSpan {
    pub name: String,
    pub kind: DeclKind,
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
    // Container decl name (struct for fields/methods, fn for params), if any.
    pub parent: Option<String>,
}

pub const LUMEN_INT_BITS: u32 = 48;
// Sign-extend an i64 into Lumen's 48-bit integer range: mask to 48 bits, then
// shift up and arithmetic-shift back down to recover the sign. Every int op
// funnels through this so the interpreter and codegen overflow identically.
pub fn wrap48(n: i64) -> i64 {

    let masked = (n as u64) & ((1u64 << LUMEN_INT_BITS) - 1);
    let shift = 64 - LUMEN_INT_BITS;
    ((masked << shift) as i64) >> shift
}
