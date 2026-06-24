//! Recursive-descent parser with a Pratt (precedence-climbing) expression core.
//! Statements use the layout tokens from the lexer: a `:` header followed by an
//! indented block, or a single inline statement. It also sprinkles SrcLine
//! markers into the AST so later errors can point at the right source line.
use crate::ast::*;
use crate::lexer::{Tok, Token};

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    // Span side-table, populated only in spanned mode (tooling). None on the
    // normal compile path, so that path is byte-for-byte unchanged.
    decls: Option<Vec<DeclSpan>>,
    // Name of the decl currently being parsed (struct/impl target or fn), so
    // fields/params/methods can record their parent. Spanned mode only.
    cur_parent: Option<String>,
    // match-lowering: the SUBJ `let` bindings to emit before a lowered `match`,
    // and a counter for the fresh temp names. See plan/match-design.md.
    mprelude: Vec<Stmt>,
    mtemp: usize,
}

type PResult<T> = Result<T, String>;

impl Parser {
    pub fn new(toks: Vec<Token>) -> Self {
        Parser {
            toks,
            pos: 0,
            decls: None,
            cur_parent: None,
            mprelude: Vec::new(),
            mtemp: 0,
        }
    }

    // Enable span collection (tooling). Returns collected decls after parsing.
    pub fn new_spanned(toks: Vec<Token>) -> Self {
        Parser {
            toks,
            pos: 0,
            decls: Some(Vec::new()),
            cur_parent: None,
            mprelude: Vec::new(),
            mtemp: 0,
        }
    }

    pub fn take_decls(&mut self) -> Vec<DeclSpan> {
        self.decls.take().unwrap_or_default()
    }

    // Record a decl span if collecting. `tok_idx` is the name token's index.
    // The lexer stamps `col` AFTER scanning a token, so token.col is the
    // exclusive END column; the start is end - byte length. Idents are ASCII,
    // so byte length == char count here.
    fn rec(&mut self, name: &str, kind: DeclKind, tok_idx: usize, parent: Option<String>) {
        if self.decls.is_none() {
            return;
        }
        let t = &self.toks[tok_idx];
        let end = t.col;
        let start = end.saturating_sub(name.len());
        if let Some(v) = self.decls.as_mut() {
            v.push(DeclSpan {
                name: name.to_string(),
                kind,
                line: t.line,
                col: start,
                end_col: end,
                parent,
            });
        }
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn line(&self) -> usize {
        self.toks[self.pos].line
    }
    fn advance(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn check(&self, t: &Tok) -> bool {
        self.peek() == t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.check(t) {
            self.advance();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, t: &Tok) -> PResult<()> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(format!(
                "line {}: expected {:?}, found {:?}",
                self.line(),
                t,
                self.peek()
            ))
        }
    }
    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Tok::Newline) {
            self.advance();
        }
    }

    // A statement must end at a line boundary: Newline (also produced by `;`),
    // Dedent, or Eof. Anything else means trailing junk on the line (e.g.
    // `return x  999`), which we reject instead of silently starting a new stmt.
    fn end_stmt(&mut self) -> PResult<()> {
        if matches!(self.peek(), Tok::Newline | Tok::Dedent | Tok::Eof) {
            Ok(())
        } else {
            Err(format!(
                "line {}: unexpected {:?} after statement (expected end of line)",
                self.line(),
                self.peek()
            ))
        }
    }

    pub fn parse_program(&mut self) -> PResult<Program> {
        let mut items = Vec::new();
        self.skip_newlines();
        while !matches!(self.peek(), Tok::Eof) {
            let line = self.line() as u32;
            let item = self.parse_item()?;

            if let Item::Stmt(_) = &item {
                items.push(Item::Stmt(Stmt::SrcLine(line)));
            }
            // A top-level `match` may have left a SUBJ `let` in the prelude;
            // emit it as an item before the lowered statement.
            for p in self.mprelude.drain(..) {
                items.push(Item::Stmt(p));
            }
            items.push(item);
            self.skip_newlines();
        }
        Ok(items)
    }

    fn parse_item(&mut self) -> PResult<Item> {
        match self.peek() {
            Tok::Export => {
                self.advance();
                self.expect(&Tok::Fn)?;
                Ok(Item::Fn(self.parse_fn(true, false)?))
            }
            Tok::Fn => {
                self.advance();
                Ok(Item::Fn(self.parse_fn(false, false)?))
            }
            Tok::Struct => Ok(Item::Struct(self.parse_struct()?)),
            Tok::Extern => Ok(Item::ExternBlock(self.parse_extern()?)),
            Tok::Import | Tok::From => Ok(Item::Import(self.parse_import()?)),
            Tok::Impl => self.parse_impl(),
            _ => Ok(Item::Stmt(self.parse_stmt()?)),
        }
    }

    fn parse_type(&mut self) -> PResult<Type> {
        match self.advance() {
            Tok::Ident(s) => Ok(Type::Named(s)),
            Tok::Dynamic => Ok(Type::Dynamic),
            Tok::Nil => Ok(Type::Nil),
            Tok::SelfKw => Ok(Type::Named("Self".into())),
            other => Err(format!(
                "line {}: expected type, found {:?}",
                self.line(),
                other
            )),
        }
    }

    fn parse_params(&mut self) -> PResult<(Vec<Param>, bool)> {
        self.expect(&Tok::LParen)?;
        let mut params = Vec::new();
        let mut is_method = false;
        if !self.check(&Tok::RParen) {
            loop {
                if self.check(&Tok::SelfKw) {
                    self.advance();
                    is_method = true;
                    params.push(Param {
                        name: "self".into(),
                        ty: Type::Named("Self".into()),
                        default: None,
                    });
                } else {
                    let (name, nidx) = self.ident_idx()?;
                    let parent = self.cur_parent.clone();
                    self.rec(&name, DeclKind::Param, nidx, parent);
                    let ty = if self.eat(&Tok::Colon) {
                        self.parse_type()?
                    } else {
                        Type::Unknown
                    };
                    // Optional default value: `name = expr`. Makes the arg
                    // omittable at call sites (defaults pad trailing args).
                    let default = if self.eat(&Tok::Assign) {
                        Some(self.parse_expr()?)
                    } else {
                        None
                    };
                    params.push(Param { name, ty, default });
                }
                if !self.comma_continues(&Tok::RParen) {
                    break;
                }
            }
        }
        self.expect(&Tok::RParen)?;
        Ok((params, is_method))
    }

    fn parse_fn(&mut self, exported: bool, in_impl: bool) -> PResult<FnDef> {
        let (name, nidx) = self.ident_idx()?;
        let kind = if in_impl {
            DeclKind::Method
        } else {
            DeclKind::Fn
        };
        let parent = if in_impl {
            self.cur_parent.clone()
        } else {
            None
        };
        self.rec(&name, kind, nidx, parent);
        // params record against this fn as parent (spanned mode only)
        let prev = self.cur_parent.take();
        if self.decls.is_some() {
            self.cur_parent = Some(name.clone());
        }
        let (params, is_method) = self.parse_params()?;
        self.cur_parent = prev;
        let ret = if self.eat(&Tok::Arrow) {
            self.parse_type()?
        } else {
            Type::Unknown
        };

        // Two body forms: `fn f() = expr` is sugar for a single-return body,
        // otherwise a `:` header introduces an indented block.
        let body = if self.eat(&Tok::Assign) {
            let e = self.parse_expr()?;
            vec![Stmt::Return(Some(e))]
        } else {
            self.expect(&Tok::Colon)?;
            self.parse_body()?
        };
        Ok(FnDef {
            name,
            params,
            ret,
            body,
            exported,
            is_method,
        })
    }

    fn parse_struct(&mut self) -> PResult<StructDef> {
        self.expect(&Tok::Struct)?;
        let (name, nidx) = self.ident_idx()?;
        self.rec(&name, DeclKind::Struct, nidx, None);
        self.expect(&Tok::Colon)?;
        self.expect(&Tok::Newline)?;
        self.expect(&Tok::Indent)?;
        let mut fields = Vec::new();
        while !self.check(&Tok::Dedent) {
            let (fname, fidx) = self.ident_idx()?;
            self.rec(&fname, DeclKind::Field, fidx, Some(name.clone()));
            self.expect(&Tok::Colon)?;
            let ty = self.parse_type()?;
            fields.push(Field { name: fname, ty });
            self.skip_newlines();
        }
        self.expect(&Tok::Dedent)?;
        Ok(StructDef {
            name,
            fields,
            methods: Vec::new(),
        })
    }

    fn parse_impl(&mut self) -> PResult<Item> {
        self.expect(&Tok::Impl)?;

        let (first, fidx) = self.ident_idx()?;
        let (target, tidx) = if self.eat(&Tok::For) {
            self.ident_idx()?
        } else {
            (first, fidx)
        };
        // Record the impl target as a Struct reference site (methods link to it).
        self.rec(&target, DeclKind::Struct, tidx, None);
        self.expect(&Tok::Colon)?;
        self.expect(&Tok::Newline)?;
        self.expect(&Tok::Indent)?;
        let prev = self.cur_parent.take();
        if self.decls.is_some() {
            self.cur_parent = Some(target.clone());
        }
        let mut methods = Vec::new();
        while !self.check(&Tok::Dedent) {
            self.skip_newlines();
            if self.check(&Tok::Dedent) {
                break;
            }
            self.expect(&Tok::Fn)?;
            methods.push(self.parse_fn(false, true)?);
            self.skip_newlines();
        }
        self.cur_parent = prev;
        self.expect(&Tok::Dedent)?;

        Ok(Item::Struct(StructDef {
            name: target,
            fields: Vec::new(),
            methods,
        }))
    }

    fn parse_extern(&mut self) -> PResult<ExternBlock> {
        self.expect(&Tok::Extern)?;
        let abi = match self.advance() {
            Tok::Str(s) => s,
            other => {
                return Err(format!(
                    "line {}: expected ABI string, found {:?}",
                    self.line(),
                    other
                ))
            }
        };
        self.expect(&Tok::From)?;
        let lib = match self.advance() {
            Tok::Str(s) => s,
            other => {
                return Err(format!(
                    "line {}: expected library string, found {:?}",
                    self.line(),
                    other
                ))
            }
        };
        self.expect(&Tok::Colon)?;
        self.expect(&Tok::Newline)?;
        self.expect(&Tok::Indent)?;
        let mut fns = Vec::new();
        while !self.check(&Tok::Dedent) {
            self.skip_newlines();
            if self.check(&Tok::Dedent) {
                break;
            }
            self.expect(&Tok::Fn)?;
            let (name, nidx) = self.ident_idx()?;
            // Extern fns are callable named decls - record like a normal fn so
            // tooling navigation matches. Params record against this fn.
            self.rec(&name, DeclKind::Fn, nidx, None);
            let prev = self.cur_parent.take();
            if self.decls.is_some() {
                self.cur_parent = Some(name.clone());
            }
            let (params, _) = self.parse_params()?;
            self.cur_parent = prev;
            let ret = if self.eat(&Tok::Arrow) {
                self.parse_type()?
            } else {
                Type::Nil
            };
            fns.push(ExternFn { name, params, ret });
            self.skip_newlines();
        }
        self.expect(&Tok::Dedent)?;
        Ok(ExternBlock { abi, lib, fns })
    }

    fn parse_import(&mut self) -> PResult<ImportDef> {
        if self.eat(&Tok::From) {
            let level = self.leading_dots();
            let module = if level > 0 && !self.check_ident() {
                String::new() // `from .. import x` - dir itself, no module seg
            } else {
                self.dotted_module()?
            };
            self.expect(&Tok::Import)?;
            let mut names = Vec::new();
            loop {
                let (nm, idx) = self.ident_idx()?;
                self.rec(&nm, DeclKind::Import, idx, Some(module.clone()));
                names.push(nm);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            Ok(ImportDef {
                module,
                alias: None,
                names,
                level,
            })
        } else {
            self.expect(&Tok::Import)?;
            let level = self.leading_dots();
            let midx = self.pos;
            let module = self.dotted_module()?;
            // Anchor the span to the first segment's token (clean single-token
            // span); dotted tails aren't separately navigable.
            let seg = module.split('.').next().unwrap_or(&module).to_string();
            self.rec(&seg, DeclKind::Import, midx, None);
            let alias = if self.eat(&Tok::As) {
                Some(self.ident()?)
            } else {
                None
            };
            Ok(ImportDef {
                module,
                alias,
                names: Vec::new(),
                level,
            })
        }
    }

    // Count + consume leading dots of a relative import. The lexer packs dots as
    // DotDot (2) and Dot (1), so `...x` is DotDot+Dot = level 3.
    fn leading_dots(&mut self) -> u8 {
        let mut n = 0u8;
        loop {
            if self.eat(&Tok::DotDot) {
                n += 2;
            } else if self.eat(&Tok::Dot) {
                n += 1;
            } else {
                break;
            }
        }
        n
    }

    fn check_ident(&self) -> bool {
        self.pos < self.toks.len() && matches!(&self.toks[self.pos].tok, Tok::Ident(_))
    }

    fn dotted_module(&mut self) -> PResult<String> {
        let mut path = self.ident()?;
        while self.eat(&Tok::Dot) {
            path.push('.');
            path.push_str(&self.ident()?);
        }
        Ok(path)
    }

    fn comma_continues(&mut self, close: &Tok) -> bool {
        if !self.eat(&Tok::Comma) {
            return false;
        }
        !self.check(close)
    }

    fn parse_block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect(&Tok::Newline)?;
        self.expect(&Tok::Indent)?;
        let mut stmts = Vec::new();
        while !self.check(&Tok::Dedent) && !self.check(&Tok::Eof) {
            self.skip_newlines();
            if self.check(&Tok::Dedent) || self.check(&Tok::Eof) {
                break;
            }
            stmts.push(Stmt::SrcLine(self.line() as u32));
            let s = self.parse_stmt()?;
            stmts.append(&mut self.mprelude);
            stmts.push(s);
            self.skip_newlines();
        }
        self.expect(&Tok::Dedent)?;
        Ok(stmts)
    }

    // A `:` body is either a newline+indented block or a single inline statement
    // on the same line. The inline case still records a SrcLine for diagnostics.
    fn parse_body(&mut self) -> PResult<Vec<Stmt>> {
        if self.check(&Tok::Newline) {
            return self.parse_block();
        }
        let line = self.line() as u32;
        let stmt = self.parse_stmt()?;
        self.skip_newlines();
        let mut out = vec![Stmt::SrcLine(line)];
        out.append(&mut self.mprelude);
        out.push(stmt);
        Ok(out)
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        // Compound statements own their block (and its Dedent), so they're
        // self-terminating and return early. Leaf statements build `stmt` and
        // fall through to a shared end-of-line check that rejects trailing junk.
        let stmt = match self.peek() {
            Tok::If => return self.parse_if(),
            Tok::Match => return self.parse_match(),
            Tok::While => {
                self.advance();
                let cond = self.parse_expr()?;
                self.expect(&Tok::Colon)?;
                let body = self.parse_block()?;
                return Ok(Stmt::While { cond, body });
            }
            Tok::For => {
                self.advance();
                let var = self.ident()?;
                // `for a, b in xs:` destructures each element into a, b, ...
                if self.check(&Tok::Comma) {
                    let mut names = vec![var];
                    while self.eat(&Tok::Comma) {
                        names.push(self.ident()?);
                    }
                    self.expect(&Tok::In)?;
                    let iter = self.parse_expr()?;
                    self.expect(&Tok::Colon)?;
                    let mut body = self.parse_block()?;
                    // Bind the loop temp's elements at the top of the body.
                    let t = format!("#d{}", self.mtemp);
                    self.mtemp += 1;
                    let mut binds = Vec::with_capacity(names.len());
                    for (i, n) in names.into_iter().enumerate() {
                        binds.push(Stmt::Let {
                            name: n,
                            mutable: false,
                            ty: Type::Unknown,
                            value: Expr::Index {
                                obj: Box::new(Expr::Ident(t.clone())),
                                index: Box::new(Expr::Int(i as i64)),
                            },
                        });
                    }
                    binds.append(&mut body);
                    return Ok(Stmt::For {
                        var: t,
                        iter,
                        body: binds,
                    });
                }
                self.expect(&Tok::In)?;
                let iter = self.parse_expr()?;
                self.expect(&Tok::Colon)?;
                let body = self.parse_block()?;
                return Ok(Stmt::For { var, iter, body });
            }
            Tok::Try => {
                self.advance();
                self.expect(&Tok::Colon)?;
                let body = self.parse_block()?;
                self.skip_newlines();
                self.expect(&Tok::Catch)?;

                let catch_var = self.ident()?;
                self.expect(&Tok::Colon)?;
                let catch_body = self.parse_block()?;
                return Ok(Stmt::Try {
                    body,
                    catch_var,
                    catch_body,
                });
            }
            Tok::Let | Tok::Mut => {
                let mutable = matches!(self.advance(), Tok::Mut);
                let name = self.ident()?;
                // Destructuring: `let a, b, ... = expr` binds each element of a
                // list/tuple-shaped RHS. Lowered here to a temp + indexed lets
                // (parser-only, so backends stay byte-identical).
                if self.check(&Tok::Comma) {
                    let mut names = vec![name];
                    while self.eat(&Tok::Comma) {
                        names.push(self.ident()?);
                    }
                    self.expect(&Tok::Assign)?;
                    let value = self.parse_expr()?;
                    return self.lower_destructure(names, value, mutable);
                }
                let ty = if self.eat(&Tok::Colon) {
                    self.parse_type()?
                } else {
                    Type::Unknown
                };
                self.expect(&Tok::Assign)?;
                let value = self.parse_expr()?;

                Stmt::Let {
                    name,
                    mutable,
                    ty,
                    value,
                }
            }
            Tok::Return => {
                self.advance();
                if matches!(self.peek(), Tok::Newline | Tok::Dedent | Tok::Eof) {
                    Stmt::Return(None)
                } else {
                    Stmt::Return(Some(self.parse_expr()?))
                }
            }
            Tok::Break => {
                self.advance();
                Stmt::Break
            }
            Tok::Continue => {
                self.advance();
                Stmt::Continue
            }
            Tok::Raise => {
                self.advance();
                let e = self.parse_expr()?;
                Stmt::Raise(e)
            }
            _ => {
                let e = self.parse_expr()?;
                if self.eat(&Tok::Assign) {
                    let value = self.parse_expr()?;
                    Stmt::Assign { target: e, value }
                } else if let Some(op) = self.compound_op() {
                    let rhs = self.parse_expr()?;
                    let value = Expr::Binary {
                        op,
                        lhs: Box::new(e.clone()),
                        rhs: Box::new(rhs),
                    };
                    Stmt::Assign { target: e, value }
                } else {
                    Stmt::ExprStmt(e)
                }
            }
        };
        // Leaf statement must end the line; trailing tokens are an error.
        self.end_stmt()?;
        Ok(stmt)
    }

    fn compound_op(&mut self) -> Option<BinOp> {
        let op = match self.peek() {
            Tok::PlusEq => BinOp::Add,
            Tok::MinusEq => BinOp::Sub,
            Tok::StarEq => BinOp::Mul,
            Tok::SlashEq => BinOp::Div,
            _ => return None,
        };
        self.advance();
        Some(op)
    }

    fn parse_if(&mut self) -> PResult<Stmt> {
        self.expect(&Tok::If)?;
        let cond = self.parse_expr()?;
        self.expect(&Tok::Colon)?;
        let then = self.parse_block()?;
        let mut elifs = Vec::new();
        let mut els = None;
        loop {
            self.skip_newlines();
            if self.eat(&Tok::Elif) {
                let c = self.parse_expr()?;
                self.expect(&Tok::Colon)?;
                let b = self.parse_block()?;
                elifs.push((c, b));
            } else if self.eat(&Tok::Else) {
                self.expect(&Tok::Colon)?;
                els = Some(self.parse_block()?);
                break;
            } else {
                break;
            }
        }
        Ok(Stmt::If {
            cond,
            then,
            elifs,
            els,
        })
    }

    // `let a, b, c = expr`: bind RHS to a temp once, then bind each name to an
    // indexed element. The temp + all-but-last bindings go through mprelude (so
    // the enclosing block emits them first); the last binding is returned.
    fn lower_destructure(
        &mut self,
        names: Vec<String>,
        value: Expr,
        mutable: bool,
    ) -> PResult<Stmt> {
        let t = format!("#d{}", self.mtemp);
        self.mtemp += 1;
        self.mprelude.push(Stmt::Let {
            name: t.clone(),
            mutable: false,
            ty: Type::Unknown,
            value,
        });
        let n = names.len();
        for (i, name) in names.into_iter().enumerate() {
            let bind = Stmt::Let {
                name,
                mutable,
                ty: Type::Unknown,
                value: Expr::Index {
                    obj: Box::new(Expr::Ident(t.clone())),
                    index: Box::new(Expr::Int(i as i64)),
                },
            };
            if i + 1 == n {
                return Ok(bind);
            }
            self.mprelude.push(bind);
        }
        unreachable!("destructure always has at least one name")
    }

    // match SUBJ: / case PAT [, PAT...] [if GUARD]: BODY / case _ : BODY
    // Lowered entirely to a Stmt::If chain here, so no backend sees `match`
    // (3-way byte-identical is automatic). See plan/match-design.md.
    fn parse_match(&mut self) -> PResult<Stmt> {
        self.expect(&Tok::Match)?;
        let subj = self.parse_expr()?;
        self.expect(&Tok::Colon)?;
        self.expect(&Tok::Newline)?;
        self.expect(&Tok::Indent)?;

        // Bind SUBJ to a fresh temp unless it is already trivial to re-read.
        // The binding is held locally and pushed to mprelude only at the very
        // end: case bodies call parse_body, which drains mprelude, so pushing
        // early would let the first case steal our SUBJ `let`.
        let trivial = matches!(
            subj,
            Expr::Ident(_)
                | Expr::Int(_)
                | Expr::Str(_)
                | Expr::Bool(_)
                | Expr::Nil
                | Expr::SelfExpr
        );
        let mut subj_let: Option<Stmt> = None;
        let subref: Expr = if trivial {
            subj.clone()
        } else {
            let t = format!("#m{}", self.mtemp);
            self.mtemp += 1;
            subj_let = Some(Stmt::Let {
                name: t.clone(),
                mutable: false,
                ty: Type::Unknown,
                value: subj,
            });
            Expr::Ident(t)
        };

        // Collect arms as (Option<cond>, body). cond=None marks the default
        // (`_` or a binding pattern), which can appear only as the last arm.
        let mut arms: Vec<(Option<Expr>, Vec<Stmt>)> = Vec::new();
        let mut seen_default = false;
        loop {
            self.skip_newlines();
            if self.check(&Tok::Dedent) || self.check(&Tok::Eof) {
                break;
            }
            self.expect(&Tok::Case)?;
            if seen_default {
                return Err("a case after `_`/binding is unreachable".into());
            }

            // First pattern: a binding/wildcard ident, or a value to compare.
            let mut bind: Option<String> = None;
            let mut alts: Vec<Expr> = Vec::new();
            if let Tok::Ident(name) = self.peek().clone() {
                // bare ident, NOT followed by a member/call/index, is a binding
                let next = &self.toks[self.pos + 1].tok;
                if matches!(next, Tok::Colon | Tok::If | Tok::Comma) {
                    bind = Some(name);
                    self.advance();
                } else {
                    alts.push(self.parse_bp(0)?);
                }
            } else {
                alts.push(self.parse_bp(0)?);
            }
            // or-pattern: more comma-separated values (only for value patterns)
            while bind.is_none() && self.eat(&Tok::Comma) {
                alts.push(self.parse_bp(0)?);
            }
            // optional guard
            let guard = if self.eat(&Tok::If) {
                Some(self.parse_bp(0)?)
            } else {
                None
            };
            self.expect(&Tok::Colon)?;
            let mut body = self.parse_body()?;

            // Build this arm's condition.
            let is_wild = bind.as_deref() == Some("_");
            if let Some(name) = &bind {
                // binding/wildcard: irrefutable default. A non-`_` name binds SUBJ.
                if !is_wild {
                    body.insert(
                        0,
                        Stmt::Let {
                            name: name.clone(),
                            mutable: false,
                            ty: Type::Unknown,
                            value: subref.clone(),
                        },
                    );
                }
                match guard {
                    // guarded default stays refutable (cond = guard)
                    Some(g) => arms.push((Some(g), body)),
                    None => {
                        seen_default = true;
                        arms.push((None, body));
                    }
                }
            } else {
                // value pattern(s): SUBJ == a [or SUBJ == b ...]
                let mut cond = eq_chain(&subref, &alts);
                if let Some(g) = guard {
                    cond = Expr::Binary {
                        op: BinOp::And,
                        lhs: Box::new(cond),
                        rhs: Box::new(g),
                    };
                }
                arms.push((Some(cond), body));
            }
        }
        self.expect(&Tok::Dedent)?;

        // Fold arms into an If chain. Leading guarded/value arms -> cond/elifs,
        // a trailing default (cond=None) -> els.
        let mut els: Option<Vec<Stmt>> = None;
        if matches!(arms.last(), Some((None, _))) {
            els = Some(arms.pop().unwrap().1);
        }
        let stmt = if arms.is_empty() {
            // only a default (or nothing): run it unconditionally (or no-op)
            Stmt::If {
                cond: Expr::Bool(true),
                then: els.unwrap_or_default(),
                elifs: Vec::new(),
                els: None,
            }
        } else {
            let (c0, b0) = arms.remove(0);
            let elifs: Vec<(Expr, Vec<Stmt>)> =
                arms.into_iter().map(|(c, b)| (c.unwrap(), b)).collect();
            Stmt::If {
                cond: c0.unwrap(),
                then: b0,
                elifs,
                els,
            }
        };
        // Now that all case bodies are parsed (and have drained their own
        // preludes), publish the SUBJ binding so the enclosing block emits it
        // right before this lowered `if`.
        if let Some(sl) = subj_let {
            self.mprelude.push(sl);
        }
        Ok(stmt)
    }

    fn parse_expr(&mut self) -> PResult<Expr> {
        let value = self.parse_bp(0)?;

        if self.eat(&Tok::If) {
            let cond = self.parse_bp(0)?;
            self.expect(&Tok::Else)?;
            let els = self.parse_expr()?;
            return Ok(Expr::IfElse {
                cond: Box::new(cond),
                then: Box::new(value),
                els: Box::new(els),
            });
        }
        Ok(value)
    }

    fn prefix_bp(op: &Tok) -> Option<u8> {
        match op {
            Tok::Minus | Tok::Not => Some(13),
            _ => None,
        }
    }

    // Binding powers for infix operators as (left_bp, right_bp). Higher binds
    // tighter; left < right means left-associative. StarStar uses (16, 15) so it
    // is right-associative (2 ** 3 ** 2 groups as 2 ** (3 ** 2)).
    fn infix_bp(op: &Tok) -> Option<(u8, u8)> {
        Some(match op {
            Tok::Or => (1, 2),
            Tok::And => (3, 4),
            Tok::EqEq | Tok::NotEq => (5, 6),
            Tok::Lt | Tok::LtEq | Tok::Gt | Tok::GtEq => (7, 8),

            Tok::In => (7, 8),
            Tok::Not => (7, 8),
            Tok::DotDot => (9, 10),
            Tok::Plus | Tok::Minus => (11, 12),
            Tok::Star | Tok::Slash | Tok::Percent => (13, 14),

            Tok::StarStar => (16, 15),
            _ => return None,
        })
    }

    fn parse_bp(&mut self, min_bp: u8) -> PResult<Expr> {
        let mut lhs = if let Some(bp) = Self::prefix_bp(self.peek()) {
            let op = self.advance();
            let rhs = self.parse_bp(bp)?;
            let uop = if matches!(op, Tok::Minus) {
                UnOp::Neg
            } else {
                UnOp::Not
            };
            Expr::Unary {
                op: uop,
                expr: Box::new(rhs),
            }
        } else {
            self.parse_postfix()?
        };

        loop {
            let op = self.peek().clone();
            let Some((lbp, rbp)) = Self::infix_bp(&op) else {
                break;
            };
            if lbp < min_bp {
                break;
            }
            self.advance();
            if matches!(op, Tok::DotDot) {
                let rhs = self.parse_bp(rbp)?;
                lhs = Expr::Range {
                    lo: Box::new(lhs),
                    hi: Box::new(rhs),
                };
                continue;
            }

            if matches!(op, Tok::Not) {
                self.expect(&Tok::In)?;
                let rhs = self.parse_bp(rbp)?;
                lhs = Expr::Binary {
                    op: BinOp::NotIn,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                };
                continue;
            }
            let rhs = self.parse_bp(rbp)?;
            let bop = match op {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                Tok::Star => BinOp::Mul,
                Tok::StarStar => BinOp::Pow,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                Tok::EqEq => BinOp::Eq,
                Tok::NotEq => BinOp::Ne,
                Tok::Lt => BinOp::Lt,
                Tok::LtEq => BinOp::Le,
                Tok::Gt => BinOp::Gt,
                Tok::GtEq => BinOp::Ge,
                Tok::And => BinOp::And,
                Tok::Or => BinOp::Or,
                Tok::In => BinOp::In,
                _ => unreachable!(),
            };
            lhs = Expr::Binary {
                op: bop,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            match self.peek() {
                Tok::LParen => {
                    self.advance();
                    if self.is_nargs() {
                        let args = self.parse_nargs()?;
                        e = Expr::NamedCall {
                            callee: Box::new(e),
                            args,
                        };
                    } else {
                        let mut args = Vec::new();
                        if !self.check(&Tok::RParen) {
                            loop {
                                args.push(self.parse_expr()?);
                                if !self.comma_continues(&Tok::RParen) {
                                    break;
                                }
                            }
                        }
                        self.expect(&Tok::RParen)?;
                        e = Expr::Call {
                            callee: Box::new(e),
                            args,
                        };
                    }
                }
                Tok::Dot => {
                    self.advance();
                    let name = self.ident()?;
                    if self.check(&Tok::LParen) {
                        self.advance();
                        let mut args = Vec::new();
                        if !self.check(&Tok::RParen) {
                            loop {
                                args.push(self.parse_expr()?);
                                if !self.comma_continues(&Tok::RParen) {
                                    break;
                                }
                            }
                        }
                        self.expect(&Tok::RParen)?;
                        e = Expr::Method {
                            obj: Box::new(e),
                            name,
                            args,
                        };
                    } else {
                        e = Expr::Field {
                            obj: Box::new(e),
                            name,
                        };
                    }
                }
                Tok::LBracket => {
                    self.advance();

                    if self.check(&Tok::Colon) {
                        self.advance();
                        let hi = if self.check(&Tok::RBracket) {
                            None
                        } else {
                            Some(Box::new(self.parse_expr()?))
                        };
                        self.expect(&Tok::RBracket)?;
                        e = Expr::Slice {
                            obj: Box::new(e),
                            lo: None,
                            hi,
                        };
                    } else {
                        let first = self.parse_expr()?;
                        if self.eat(&Tok::Colon) {
                            let hi = if self.check(&Tok::RBracket) {
                                None
                            } else {
                                Some(Box::new(self.parse_expr()?))
                            };
                            self.expect(&Tok::RBracket)?;
                            e = Expr::Slice {
                                obj: Box::new(e),
                                lo: Some(Box::new(first)),
                                hi,
                            };
                        } else {
                            self.expect(&Tok::RBracket)?;
                            e = Expr::Index {
                                obj: Box::new(e),
                                index: Box::new(first),
                            };
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(e)
    }

    // Distinguish `f(x: 1)` (named/struct-literal args) from a positional call by
    // peeking for `Ident :` right after the open paren.
    fn is_nargs(&self) -> bool {
        matches!(&self.toks[self.pos].tok, Tok::Ident(_))
            && matches!(&self.toks[self.pos + 1].tok, Tok::Colon)
    }

    fn parse_nargs(&mut self) -> PResult<Vec<(String, Expr)>> {
        let mut args = Vec::new();
        if !self.check(&Tok::RParen) {
            loop {
                let name = self.ident()?;
                self.expect(&Tok::Colon)?;
                let val = self.parse_expr()?;
                args.push((name, val));
                if !self.comma_continues(&Tok::RParen) {
                    break;
                }
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(args)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        match self.advance() {
            Tok::Int(n) => Ok(Expr::Int(n)),
            Tok::Float(f) => Ok(Expr::Float(f)),
            Tok::Str(s) => Ok(Expr::Str(std::rc::Rc::new(s))),
            Tok::FStr(s) => Ok(Expr::FStr(parse_fstring(&s))),
            Tok::True => Ok(Expr::Bool(true)),
            Tok::False => Ok(Expr::Bool(false)),
            Tok::Nil => Ok(Expr::Nil),
            Tok::SelfKw => Ok(Expr::SelfExpr),
            Tok::Ident(s) => Ok(Expr::Ident(s)),
            Tok::Fn => {
                self.expect(&Tok::LParen)?;
                let mut params = Vec::new();
                if !self.check(&Tok::RParen) {
                    loop {
                        params.push(self.ident()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                }
                self.expect(&Tok::RParen)?;
                self.expect(&Tok::Colon)?;
                let body = if self.check(&Tok::Newline) {
                    self.parse_block()?
                } else {
                    let ex = self.parse_expr()?;
                    vec![Stmt::Return(Some(ex))]
                };
                Ok(Expr::Lambda { params, body })
            }
            Tok::LParen => {
                let e = self.parse_expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Tok::LBracket => {
                if self.check(&Tok::RBracket) {
                    self.advance();
                    return Ok(Expr::List(Vec::new()));
                }

                let first = self.parse_expr()?;
                // `[expr for x in iter if cond]` is a list comprehension; without
                // the `for` it is just a list literal. We parse the first element
                // either way, then branch on what follows.
                if self.check(&Tok::For) {
                    self.advance();
                    let var = self.ident()?;
                    self.expect(&Tok::In)?;

                    let iter = self.parse_bp(0)?;
                    let cond = if self.check(&Tok::If) {
                        self.advance();
                        Some(Box::new(self.parse_bp(0)?))
                    } else {
                        None
                    };
                    self.expect(&Tok::RBracket)?;
                    return Ok(Expr::ListComp {
                        elem: Box::new(first),
                        var,
                        iter: Box::new(iter),
                        cond,
                    });
                }
                let mut elems = vec![first];
                if self.comma_continues(&Tok::RBracket) {
                    loop {
                        elems.push(self.parse_expr()?);
                        if !self.comma_continues(&Tok::RBracket) {
                            break;
                        }
                    }
                }
                self.expect(&Tok::RBracket)?;
                Ok(Expr::List(elems))
            }
            Tok::LBrace => {
                let mut entries = Vec::new();
                if !self.check(&Tok::RBrace) {
                    loop {
                        let k = self.parse_expr()?;
                        self.expect(&Tok::Colon)?;
                        let v = self.parse_expr()?;
                        entries.push((k, v));
                        if !self.comma_continues(&Tok::RBrace) {
                            break;
                        }
                    }
                }
                self.expect(&Tok::RBrace)?;
                Ok(Expr::Map(entries))
            }
            other => Err(format!(
                "line {}: unexpected token in expression: {:?}",
                self.line(),
                other
            )),
        }
    }

    fn ident(&mut self) -> PResult<String> {
        match self.advance() {
            Tok::Ident(s) => Ok(s),
            other => Err(format!(
                "line {}: expected identifier, found {:?}",
                self.line(),
                other
            )),
        }
    }

    // Like ident(), but also returns the name token's index (for span recording).
    fn ident_idx(&mut self) -> PResult<(String, usize)> {
        let idx = self.pos;
        let s = self.ident()?;
        Ok((s, idx))
    }
}

// Build `subj == a` or `(subj == a) or (subj == b) or ...` for or-patterns.
fn eq_chain(subj: &Expr, alts: &[Expr]) -> Expr {
    let mk = |a: &Expr| Expr::Binary {
        op: BinOp::Eq,
        lhs: Box::new(subj.clone()),
        rhs: Box::new(a.clone()),
    };
    let mut it = alts.iter();
    let mut cond = mk(it.next().expect("at least one pattern"));
    for a in it {
        cond = Expr::Binary {
            op: BinOp::Or,
            lhs: Box::new(cond),
            rhs: Box::new(mk(a)),
        };
    }
    cond
}

fn parse_fstring(s: &str) -> Vec<FStrPart> {
    let mut parts = Vec::new();
    let mut lit = String::new();
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == '{' {
            if i + 1 < bytes.len() && bytes[i + 1] == '{' {
                lit.push('{');
                i += 2;
                continue;
            }

            if !lit.is_empty() {
                parts.push(FStrPart::Lit(std::mem::take(&mut lit)));
            }

            let mut depth = 1;
            let mut expr_src = String::new();
            i += 1;
            while i < bytes.len() && depth > 0 {
                let d = bytes[i];
                if d == '{' {
                    depth += 1;
                } else if d == '}' {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                expr_src.push(d);
                i += 1;
            }

            match crate::lexer::Lexer::new(&expr_src)
                .tokenize()
                .and_then(|t| Parser::new(t).parse_top())
            {
                Ok(e) => parts.push(FStrPart::Expr(e)),
                Err(_) => parts.push(FStrPart::Lit(format!("{{{}}}", expr_src))),
            }
        } else if c == '}' && i + 1 < bytes.len() && bytes[i + 1] == '}' {
            lit.push('}');
            i += 2;
        } else {
            lit.push(c);
            i += 1;
        }
    }
    if !lit.is_empty() {
        parts.push(FStrPart::Lit(lit));
    }
    parts
}

impl Parser {
    fn parse_top(&mut self) -> PResult<Expr> {
        self.skip_newlines();
        self.parse_expr()
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::DeclKind;
    use crate::parse_spanned;

    // Every decl span must slice back to exactly its own name in the source.
    #[test]
    fn spans_named() {
        let src = "from math import sqrt, pi\n\
                   struct Point:\n    x: int\n    y: int\n\
                   impl Point:\n    fn dist(self, other):\n        return sqrt(other)\n\
                   fn main(a, b):\n    print(a)\n";
        let (_, decls) = parse_spanned(src).expect("parse");
        let lines: Vec<&str> = src.split('\n').collect();
        for d in &decls {
            let l = lines[d.line - 1];
            let slice = &l[d.col - 1..d.end_col - 1];
            assert_eq!(slice, d.name, "span mismatch for {:?}", d);
        }
        // Spot-check kinds/parents are wired up.
        assert!(decls.iter().any(|d| d.kind == DeclKind::Method
            && d.name == "dist"
            && d.parent.as_deref() == Some("Point")));
        assert!(decls.iter().any(|d| d.kind == DeclKind::Param
            && d.name == "other"
            && d.parent.as_deref() == Some("dist")));
        assert!(decls
            .iter()
            .any(|d| d.kind == DeclKind::Import && d.name == "sqrt"));
    }

    // The normal (spanless) parse path must yield no decls.
    #[test]
    fn spanless_empty() {
        let toks = crate::lexer::Lexer::new("fn f():\n    return 1\n")
            .tokenize()
            .unwrap();
        let mut p = super::Parser::new(toks);
        p.parse_program().unwrap();
        assert!(p.take_decls().is_empty());
    }

    // Trailing tokens after a statement are junk and must be rejected, not
    // silently parsed as a second statement. Regression for `return x 999`,
    // `let x = 1 2`, bare-literal-then-more, `f() garbage`, etc.
    #[test]
    fn reject_junk() {
        let bad = [
            "fn f():\n    return 1 999\n",
            "fn f():\n    let x = 1 2 3\n    return x\n",
            "fn f():\n    return \"neg\" 321313\n",
            "fn main():\n    print(1) garbage\n",
            "fn main():\n    421421412241 7\n",
        ];
        for src in bad {
            assert!(
                crate::parse_program(src).is_err(),
                "should reject trailing junk: {src:?}"
            );
        }
    }

    // `;` is a statement separator (lexes to Newline), so these stay valid.
    // Compound statements remain self-terminating. Guards against over-rejection.
    #[test]
    fn stmt_ends() {
        let good = [
            "fn main():\n    print(1); print(2)\n",
            "fn main():\n    print(1);\n",
            "fn f(n: i64):\n    if n > 0:\n        return n\n    else:\n        return 0\n",
            "fn main():\n    for i in 0..3:\n        print(i)\n",
            "fn main():\n    let x = 1\n    x += 5\n",
        ];
        for src in good {
            assert!(
                crate::parse_program(src).is_ok(),
                "should accept valid stmt end: {src:?}"
            );
        }
    }

    // Leading dots on an import set the relative `level`: 0 = absolute, 1 = .x,
    // 2 = ..x, 3 = ...pkg.x. `from .x import y` carries level too.
    #[test]
    fn import_levels() {
        let cases = [
            ("import foo\n", 0u8, "foo"),
            ("import .foo\n", 1, "foo"),
            ("import ..foo\n", 2, "foo"),
            ("import ...pkg.foo\n", 3, "pkg.foo"),
            ("from .foo import bar\n", 1, "foo"),
            ("from ..pkg.foo import bar\n", 2, "pkg.foo"),
        ];
        for (src, lvl, module) in cases {
            let prog = crate::parse_program(src).unwrap();
            let imp = prog
                .iter()
                .find_map(|it| match it {
                    crate::ast::Item::Import(i) => Some(i),
                    _ => None,
                })
                .expect("an import");
            assert_eq!(imp.level, lvl, "level for {src:?}");
            assert_eq!(imp.module, module, "module for {src:?}");
        }
    }

    // A param's `= expr` is recorded as its default; bare params have none.
    #[test]
    fn param_defaults() {
        let prog = crate::parse_program("fn f(a, b=10, c=\"x\"):\n    return a\n").unwrap();
        let f = prog
            .iter()
            .find_map(|it| match it {
                crate::ast::Item::Fn(f) => Some(f),
                _ => None,
            })
            .expect("a fn");
        assert!(f.params[0].default.is_none(), "a has no default");
        assert!(f.params[1].default.is_some(), "b has a default");
        assert!(f.params[2].default.is_some(), "c has a default");
    }

    // `match` lowers to a Stmt::If: first case is the cond/then, the rest elifs,
    // a trailing `_` is the else.
    #[test]
    fn match_lowers_to_if() {
        let prog = crate::parse_program(
            "fn f(n):\n    match n:\n        case 1:\n            return 10\n        case 2, 3:\n            return 20\n        case _:\n            return 0\n",
        )
        .unwrap();
        let f = prog
            .iter()
            .find_map(|it| match it {
                crate::ast::Item::Fn(f) => Some(f),
                _ => None,
            })
            .expect("a fn");
        let has_if = f
            .body
            .iter()
            .any(|s| matches!(s, crate::ast::Stmt::If { elifs, els, .. } if elifs.len() == 1 && els.is_some()));
        assert!(
            has_if,
            "match should lower to an if/elif/else: {:?}",
            f.body
        );
    }

    // A non-trivial subject is bound to a `#m` temp before the lowered if.
    #[test]
    fn match_binds_subject_temp() {
        let prog =
            crate::parse_program("fn f(xs):\n    match xs.len():\n        case 0:\n            return 1\n        case _:\n            return 2\n")
                .unwrap();
        let f = prog
            .iter()
            .find_map(|it| match it {
                crate::ast::Item::Fn(f) => Some(f),
                _ => None,
            })
            .expect("a fn");
        let binds = f
            .body
            .iter()
            .any(|s| matches!(s, crate::ast::Stmt::Let { name, .. } if name.starts_with("#m")));
        assert!(binds, "subject should be bound to a #m temp: {:?}", f.body);
    }

    // `let a, b = e` lowers to a #d temp plus one indexed let per name.
    #[test]
    fn destructure_let() {
        let prog = crate::parse_program("fn f(p):\n    let a, b = p\n    return a\n").unwrap();
        let f = prog
            .iter()
            .find_map(|it| match it {
                crate::ast::Item::Fn(f) => Some(f),
                _ => None,
            })
            .expect("a fn");
        let temps = f
            .body
            .iter()
            .filter(|s| matches!(s, crate::ast::Stmt::Let { name, .. } if name.starts_with("#d")))
            .count();
        let named = f
            .body
            .iter()
            .filter(
                |s| matches!(s, crate::ast::Stmt::Let { name, .. } if name == "a" || name == "b"),
            )
            .count();
        assert_eq!(temps, 1, "one #d temp: {:?}", f.body);
        assert_eq!(named, 2, "a and b bound: {:?}", f.body);
    }

    // `for a, b in xs:` lowers to a for over a #d temp with element binds inside.
    #[test]
    fn destructure_for() {
        let prog =
            crate::parse_program("fn f(xs):\n    for a, b in xs:\n        print(a)\n").unwrap();
        let f = prog
            .iter()
            .find_map(|it| match it {
                crate::ast::Item::Fn(f) => Some(f),
                _ => None,
            })
            .expect("a fn");
        let ok = f.body.iter().any(|s| matches!(
            s,
            crate::ast::Stmt::For { var, body, .. }
                if var.starts_with("#d")
                    && body.iter().filter(|b| matches!(b, crate::ast::Stmt::Let { .. })).count() >= 2
        ));
        assert!(ok, "for should destructure via #d temp: {:?}", f.body);
    }
}
