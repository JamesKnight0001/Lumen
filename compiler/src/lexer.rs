
//! Hand-written lexer for Lumen. Turns source bytes into tokens, and crucially
//! implements the off-side rule: it tracks an indentation stack and emits
//! synthetic Indent/Dedent/Newline tokens so the parser can treat blocks like
//! braces. Parens/brackets suppress that so expressions can wrap across lines.
#![allow(dead_code)]
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {

    Int(i64),
    Float(f64),
    Str(String),
    FStr(String),
    Ident(String),

    Let,
    Mut,
    Fn,
    If,
    Elif,
    Else,
    For,
    In,
    While,
    Return,
    Match,
    Case,
    Struct,
    Enum,
    Trait,
    Impl,
    Import,
    From,
    As,
    Export,
    Extern,
    TypeKw,
    Dynamic,
    Weak,
    And,
    Or,
    Not,
    True,
    False,
    Nil,
    Break,
    Continue,
    Try,
    Catch,
    Raise,
    Do,
    End,
    SelfKw,
    With,

    Plus,
    Minus,
    Star,
    StarStar,
    Slash,
    Percent,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    EqEq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Assign,
    Colon,
    Comma,
    Dot,
    Arrow,
    FatArrow,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    DotDot,

    Newline,
    Indent,
    Dedent,
    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
    pub col: usize,
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
    indents: Vec<usize>,
    pending: Vec<Token>,
    at_line_start: bool,
    // Depth of open (), [], {}. While > 0 we ignore newlines and indentation so
    // bracketed expressions can span lines without emitting layout tokens.
    paren_depth: i32,
}

fn keyword(s: &str) -> Option<Tok> {
    Some(match s {
        "let" => Tok::Let,
        "mut" => Tok::Mut,
        "fn" => Tok::Fn,
        "if" => Tok::If,
        "elif" => Tok::Elif,
        "else" => Tok::Else,
        "for" => Tok::For,
        "in" => Tok::In,
        "while" => Tok::While,
        "return" => Tok::Return,
        "match" => Tok::Match,
        "case" => Tok::Case,
        "struct" => Tok::Struct,
        "enum" => Tok::Enum,
        "trait" => Tok::Trait,
        "impl" => Tok::Impl,
        "import" => Tok::Import,
        "from" => Tok::From,
        "as" => Tok::As,
        "export" => Tok::Export,
        "extern" => Tok::Extern,
        "dynamic" => Tok::Dynamic,
        "weak" => Tok::Weak,
        "and" => Tok::And,
        "or" => Tok::Or,
        "not" => Tok::Not,
        "true" => Tok::True,
        "false" => Tok::False,
        "nil" => Tok::Nil,
        "break" => Tok::Break,
        "continue" => Tok::Continue,
        "try" => Tok::Try,
        "catch" => Tok::Catch,
        "raise" => Tok::Raise,
        "do" => Tok::Do,
        "end" => Tok::End,
        "self" => Tok::SelfKw,
        "with" => Tok::With,
        _ => return None,
    })
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            src: src.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            indents: vec![0],
            pending: Vec::new(),
            at_line_start: true,
            paren_depth: 0,
        }
    }

    fn peek(&self) -> u8 {
        if self.pos < self.src.len() {
            self.src[self.pos]
        } else {
            0
        }
    }
    fn peek2(&self) -> u8 {
        if self.pos + 1 < self.src.len() {
            self.src[self.pos + 1]
        } else {
            0
        }
    }
    fn bump(&mut self) -> u8 {
        let c = self.peek();
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        c
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>, String> {
        let mut out = Vec::new();
        loop {
            let t = self.next_token()?;
            let eof = t.tok == Tok::Eof;
            out.push(t);
            if eof {
                break;
            }
        }
        Ok(out)
    }

    fn mk(&self, tok: Tok) -> Token {
        Token {
            tok,
            line: self.line,
            col: self.col,
        }
    }

    fn next_token(&mut self) -> Result<Token, String> {
        if let Some(t) = self.pending.pop() {
            return Ok(t);
        }

        while self.at_line_start && self.paren_depth == 0 {
            let before = self.at_line_start;
            if let Some(t) = self.handle_line()? {
                return Ok(t);
            }

            if before && !self.at_line_start {
                break;
            }

            if self.peek() == 0 {
                break;
            }
        }

        loop {
            let c = self.peek();
            if c == b' ' || c == b'\t' || c == b'\r' {
                self.bump();
            } else if c == b'#' {
                if self.peek2() == b'[' {
                    self.skip_block();
                } else {
                    while self.pos < self.src.len() && self.peek() != b'\n' {
                        self.bump();
                    }
                }
            } else {
                break;
            }
        }

        let c = self.peek();
        if c == 0 {
            // At end of input, flush any still-open indentation as Dedents
            // before the final Eof so the parser sees balanced blocks.
            if self.indents.len() > 1 {
                self.indents.pop();
                return Ok(self.mk(Tok::Dedent));
            }
            return Ok(self.mk(Tok::Eof));
        }

        if c == b'\n' {
            self.bump();
            if self.paren_depth == 0 {
                self.at_line_start = true;
                return Ok(self.mk(Tok::Newline));
            } else {

                return self.next_token();
            }
        }

        // f"..." / f'...' starts an f-string, not the identifier "f"; this
        // branch checks for the prefix before scanning a normal identifier.
        if c.is_ascii_alphabetic() || c == b'_' {

            if (c == b'f' || c == b'F') && (self.peek2() == b'"' || self.peek2() == b'\'') {
                self.bump();
                return self.lex_string(true);
            }
            let start = self.pos;
            while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                self.bump();
            }
            let s = std::str::from_utf8(&self.src[start..self.pos])
                .unwrap()
                .to_string();
            if let Some(kw) = keyword(&s) {
                return Ok(self.mk(kw));
            }
            return Ok(self.mk(Tok::Ident(s)));
        }

        if c.is_ascii_digit() {
            return self.lex_number();
        }

        if c == b'"' || c == b'\'' {
            return self.lex_string(false);
        }

        self.bump();
        let tok = match c {
            b'+' => {
                if self.peek() == b'=' {
                    self.bump();
                    Tok::PlusEq
                } else {
                    Tok::Plus
                }
            }
            b'-' => {
                if self.peek() == b'>' {
                    self.bump();
                    Tok::Arrow
                } else if self.peek() == b'=' {
                    self.bump();
                    Tok::MinusEq
                } else {
                    Tok::Minus
                }
            }
            b'*' => {
                if self.peek() == b'*' {
                    self.bump();
                    Tok::StarStar
                } else if self.peek() == b'=' {
                    self.bump();
                    Tok::StarEq
                } else {
                    Tok::Star
                }
            }
            b'/' => {
                if self.peek() == b'=' {
                    self.bump();
                    Tok::SlashEq
                } else {
                    Tok::Slash
                }
            }
            b'%' => Tok::Percent,
            b'(' => {
                self.paren_depth += 1;
                Tok::LParen
            }
            b')' => {
                self.paren_depth -= 1;
                Tok::RParen
            }
            b'[' => {
                self.paren_depth += 1;
                Tok::LBracket
            }
            b']' => {
                self.paren_depth -= 1;
                Tok::RBracket
            }
            b'{' => {
                self.paren_depth += 1;
                Tok::LBrace
            }
            b'}' => {
                self.paren_depth -= 1;
                Tok::RBrace
            }
            b':' => Tok::Colon,
            b',' => Tok::Comma,
            b'.' => {
                if self.peek() == b'.' {
                    self.bump();
                    Tok::DotDot
                } else {
                    Tok::Dot
                }
            }
            b'=' => {
                if self.peek() == b'=' {
                    self.bump();
                    Tok::EqEq
                } else if self.peek() == b'>' {
                    self.bump();
                    Tok::FatArrow
                } else {
                    Tok::Assign
                }
            }
            b'!' => {
                if self.peek() == b'=' {
                    self.bump();
                    Tok::NotEq
                } else {
                    return Err(format!("line {}: unexpected '!'", self.line));
                }
            }
            b'<' => {
                if self.peek() == b'=' {
                    self.bump();
                    Tok::LtEq
                } else {
                    Tok::Lt
                }
            }
            b'>' => {
                if self.peek() == b'=' {
                    self.bump();
                    Tok::GtEq
                } else {
                    Tok::Gt
                }
            }
            b';' => Tok::Newline,
            _ => {
                return Err(format!(
                    "line {}: unexpected character {:?}",
                    self.line, c as char
                ))
            }
        };
        Ok(self.mk(tok))
    }

    // Skip a `#[ ... ]#` block comment, delimiters included, across any number
    // of lines. Cursor must be on the opening `#`; unterminated runs to EOF.
    fn skip_block(&mut self) {
        self.bump(); // #
        self.bump(); // [
        while self.pos < self.src.len() {
            if self.peek() == b']' && self.peek2() == b'#' {
                self.bump();
                self.bump();
                break;
            }
            self.bump();
        }
    }

    // At line start: measure indent width, skip blank/comment-only lines, else
    // emit the layout token (Indent/Dedent/none) for this line.
    fn handle_line(&mut self) -> Result<Option<Token>, String> {
        let mut width = 0usize;
        loop {
            match self.peek() {
                b' ' => {
                    width += 1;
                    self.bump();
                }
                b'\t' => {
                    width += 1;
                    self.bump();
                }
                _ => break,
            }
        }

        let c = self.peek();
        // A block comment can span lines, so `#[` consumes through its `]#` even
        // across newlines, unlike a `#` line comment.
        if c == b'#' && self.peek2() == b'[' {
            self.skip_block();
            // Skip trailing space; if the line is now empty it carries no layout,
            // otherwise emit indent and let the caller tokenize the rest.
            while self.peek() == b' ' || self.peek() == b'\t' || self.peek() == b'\r' {
                self.bump();
            }
            let after = self.peek();
            if after == b'\n' {
                self.bump();
                self.at_line_start = true;
                return Ok(None);
            }
            if after == 0 {
                self.at_line_start = true;
                return Ok(None);
            }
            self.at_line_start = false;
            return self.emit_indent(width);
        }
        if c == b'\n' || c == b'#' || c == 0 {
            if c == b'\n' {
                self.bump();
            } else if c == b'#' {
                while self.pos < self.src.len() && self.peek() != b'\n' {
                    self.bump();
                }
                if self.peek() == b'\n' {
                    self.bump();
                }
            }

            self.at_line_start = true;
            return Ok(None);
        }

        self.at_line_start = false;
        self.emit_indent(width)
    }

    // Emit the layout token for `width` vs the indent stack: Indent, Dedent(s),
    // or none. Shared so handle_line can reuse it after a leading block comment.
    fn emit_indent(&mut self, width: usize) -> Result<Option<Token>, String> {
        let cur = *self.indents.last().unwrap();
        if width > cur {
            self.indents.push(width);
            return Ok(Some(self.mk(Tok::Indent)));
        } else if width < cur {
            let mut emitted = None;
            while width < *self.indents.last().unwrap() {
                self.indents.pop();
                let d = self.mk(Tok::Dedent);
                if emitted.is_none() {
                    emitted = Some(d);
                } else {
                    self.pending.push(d);
                }
            }
            if width != *self.indents.last().unwrap() {
                return Err(format!("line {}: inconsistent indentation", self.line));
            }
            return Ok(emitted);
        }
        Ok(None)
    }

    fn lex_number(&mut self) -> Result<Token, String> {
        let start = self.pos;
        while self.peek().is_ascii_digit() || self.peek() == b'_' {
            self.bump();
        }
        let mut is_float = false;
        if self.peek() == b'.' && self.peek2().is_ascii_digit() {
            is_float = true;
            self.bump();
            while self.peek().is_ascii_digit() || self.peek() == b'_' {
                self.bump();
            }
        }
        if self.peek() == b'e' || self.peek() == b'E' {
            is_float = true;
            self.bump();
            if self.peek() == b'+' || self.peek() == b'-' {
                self.bump();
            }
            while self.peek().is_ascii_digit() {
                self.bump();
            }
        }
        let raw: String = std::str::from_utf8(&self.src[start..self.pos])
            .unwrap()
            .chars()
            .filter(|&c| c != '_')
            .collect();
        if is_float {
            raw.parse::<f64>()
                .map(|f| self.mk(Tok::Float(f)))
                .map_err(|e| format!("line {}: bad float: {}", self.line, e))
        } else {
            raw.parse::<i64>()
                .map(|i| self.mk(Tok::Int(i)))
                .map_err(|e| format!("line {}: bad int: {}", self.line, e))
        }
    }

    fn lex_string(&mut self, fstring: bool) -> Result<Token, String> {
        let quote = self.bump();
        let mut s = String::new();
        loop {
            let c = self.peek();
            if c == 0 {
                return Err(format!("line {}: unterminated string", self.line));
            }
            if c == quote {
                self.bump();
                break;
            }
            if c == b'\\' {
                self.bump();
                let e = self.bump();
                match e {
                    b'n' => s.push('\n'),
                    b't' => s.push('\t'),
                    b'r' => s.push('\r'),
                    b'\\' => s.push('\\'),
                    b'"' => s.push('"'),
                    b'\'' => s.push('\''),
                    b'0' => s.push('\0'),
                    b'{' => s.push('{'),
                    b'}' => s.push('}'),
                    other => {
                        s.push('\\');
                        s.push(other as char);
                    }
                }
            } else {

                s.push(self.bump() as char);
            }
        }
        Ok(self.mk(if fstring { Tok::FStr(s) } else { Tok::Str(s) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        Lexer::new(src)
            .tokenize()
            .unwrap()
            .into_iter()
            .map(|t| t.tok)
            .collect()
    }

    #[test]
    fn single_comment() {
        let t = toks("#[ hi ]#\nlet x = 1\n");
        assert!(matches!(t[0], Tok::Let));
    }

    #[test]
    fn multiline_comment() {
        // Regression: a multi-line block comment must be fully skipped (incl.
        // continuation indent) and not leak Newline/Indent tokens.
        let t = toks("#[ line one\n   line two\n   line three ]#\nlet x = 1\n");
        assert!(matches!(t[0], Tok::Let), "got {:?}", &t[..t.len().min(4)]);
    }

    #[test]
    fn block_comment() {
        let src = "fn f():\n    #[ a\n    b ]#\n    return 1\n";
        let t = toks(src);
        assert!(t.iter().any(|x| matches!(x, Tok::Return)));
        assert!(t.iter().any(|x| matches!(x, Tok::Indent)));
    }

    #[test]
    fn block_comment_code() {
        // Code after `]#` on the same line survives.
        let t = toks("#[ c ]# let x = 1\n");
        assert!(matches!(t[0], Tok::Let));
    }

    #[test]
    fn line_comment_ok() {
        let t = toks("# just a line\nlet y = 2\n");
        assert!(matches!(t[0], Tok::Let));
    }
}
