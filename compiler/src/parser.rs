
//! Recursive-descent parser with a Pratt (precedence-climbing) expression core.
//! Statements use the layout tokens from the lexer: a `:` header followed by an
//! indented block, or a single inline statement. It also sprinkles SrcLine
//! markers into the AST so later errors can point at the right source line.
use crate::ast::*;
use crate::lexer::{Tok, Token};

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

type PResult<T> = Result<T, String>;

impl Parser {
    pub fn new(toks: Vec<Token>) -> Self {
        Parser { toks, pos: 0 }
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

    pub fn parse_program(&mut self) -> PResult<Program> {
        let mut items = Vec::new();
        self.skip_newlines();
        while !matches!(self.peek(), Tok::Eof) {
            let line = self.line() as u32;
            let item = self.parse_item()?;

            if let Item::Stmt(_) = &item {
                items.push(Item::Stmt(Stmt::SrcLine(line)));
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
                    });
                } else {
                    let name = self.ident()?;
                    let ty = if self.eat(&Tok::Colon) {
                        self.parse_type()?
                    } else {
                        Type::Unknown
                    };
                    params.push(Param { name, ty });
                }
                if !self.comma_continues(&Tok::RParen) {
                    break;
                }
            }
        }
        self.expect(&Tok::RParen)?;
        Ok((params, is_method))
    }

    fn parse_fn(&mut self, exported: bool, _in_impl: bool) -> PResult<FnDef> {
        let name = self.ident()?;
        let (params, is_method) = self.parse_params()?;
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
        let name = self.ident()?;
        self.expect(&Tok::Colon)?;
        self.expect(&Tok::Newline)?;
        self.expect(&Tok::Indent)?;
        let mut fields = Vec::new();
        while !self.check(&Tok::Dedent) {
            let fname = self.ident()?;
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

        let first = self.ident()?;
        let target = if self.eat(&Tok::For) {
            self.ident()?
        } else {
            first
        };
        self.expect(&Tok::Colon)?;
        self.expect(&Tok::Newline)?;
        self.expect(&Tok::Indent)?;
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
            let name = self.ident()?;
            let (params, _) = self.parse_params()?;
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
            let module = self.dotted_module()?;
            self.expect(&Tok::Import)?;
            let mut names = Vec::new();
            loop {
                names.push(self.ident()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            Ok(ImportDef {
                module,
                alias: None,
                names,
            })
        } else {
            self.expect(&Tok::Import)?;
            let module = self.dotted_module()?;
            let alias = if self.eat(&Tok::As) {
                Some(self.ident()?)
            } else {
                None
            };
            Ok(ImportDef {
                module,
                alias,
                names: Vec::new(),
            })
        }
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
            stmts.push(self.parse_stmt()?);
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
        Ok(vec![Stmt::SrcLine(line), stmt])
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        match self.peek() {
            Tok::Let | Tok::Mut => {
                let mutable = matches!(self.advance(), Tok::Mut);
                let name = self.ident()?;
                let ty = if self.eat(&Tok::Colon) {
                    self.parse_type()?
                } else {
                    Type::Unknown
                };
                self.expect(&Tok::Assign)?;
                let value = self.parse_expr()?;
                Ok(Stmt::Let {
                    name,
                    mutable,
                    ty,
                    value,
                })
            }
            Tok::Return => {
                self.advance();
                if matches!(self.peek(), Tok::Newline | Tok::Dedent | Tok::Eof) {
                    Ok(Stmt::Return(None))
                } else {
                    Ok(Stmt::Return(Some(self.parse_expr()?)))
                }
            }
            Tok::If => self.parse_if(),
            Tok::While => {
                self.advance();
                let cond = self.parse_expr()?;
                self.expect(&Tok::Colon)?;
                let body = self.parse_block()?;
                Ok(Stmt::While { cond, body })
            }
            Tok::For => {
                self.advance();
                let var = self.ident()?;
                self.expect(&Tok::In)?;
                let iter = self.parse_expr()?;
                self.expect(&Tok::Colon)?;
                let body = self.parse_block()?;
                Ok(Stmt::For { var, iter, body })
            }
            Tok::Break => {
                self.advance();
                Ok(Stmt::Break)
            }
            Tok::Continue => {
                self.advance();
                Ok(Stmt::Continue)
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
                Ok(Stmt::Try {
                    body,
                    catch_var,
                    catch_body,
                })
            }
            Tok::Raise => {
                self.advance();
                let e = self.parse_expr()?;
                Ok(Stmt::Raise(e))
            }
            _ => {
                let e = self.parse_expr()?;
                if self.eat(&Tok::Assign) {
                    let value = self.parse_expr()?;
                    Ok(Stmt::Assign { target: e, value })
                } else if let Some(op) = self.compound_op() {

                    let rhs = self.parse_expr()?;
                    let value = Expr::Binary {
                        op,
                        lhs: Box::new(e.clone()),
                        rhs: Box::new(rhs),
                    };
                    Ok(Stmt::Assign { target: e, value })
                } else {
                    Ok(Stmt::ExprStmt(e))
                }
            }
        }
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
                    if self.is_named_args() {
                        let args = self.parse_named_args()?;
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
    fn is_named_args(&self) -> bool {

        matches!(&self.toks[self.pos].tok, Tok::Ident(_))
            && matches!(&self.toks[self.pos + 1].tok, Tok::Colon)
    }

    fn parse_named_args(&mut self) -> PResult<Vec<(String, Expr)>> {
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
            Tok::Str(s) => Ok(Expr::Str(s)),
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
                .and_then(|t| Parser::new(t).parse_expr_top())
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

    fn parse_expr_top(&mut self) -> PResult<Expr> {
        self.skip_newlines();
        self.parse_expr()
    }
}
