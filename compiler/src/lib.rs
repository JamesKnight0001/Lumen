
//! Crate root for the Lumen compiler. Wires the front end together: lex,
//! parse, resolve imports, desugar, lambda-lift, then optionally optimize.
//! `compile` is the one entry point both backends (interp, codegen) consume.
#![allow(clippy::enum_variant_names)]
#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::doc_overindented_list_items)]

pub mod ast;
pub mod builtins;
pub mod codegen;
pub mod desugar;
#[cfg(windows)]
pub mod ffi;
pub mod http;
pub mod imports;
pub mod interp;
pub mod lexer;
pub mod lift;
pub mod mir;
pub mod net;
pub mod opt;
pub mod parser;
pub mod pkg;
pub mod toolchain;
pub mod types;

use std::path::Path;

#[derive(Debug, Clone)]
pub enum CompileError {

    Lex(String),

    Parse(String),

    Import(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Lex(m) => write!(f, "lex error: {m}"),
            CompileError::Parse(m) => write!(f, "parse error: {m}"),
            CompileError::Import(m) => write!(f, "import error: {m}"),
        }
    }
}

impl std::error::Error for CompileError {}

pub fn parse_program(src: &str) -> Result<ast::Program, CompileError> {
    let toks = lexer::Lexer::new(src)
        .tokenize()
        .map_err(CompileError::Lex)?;
    parser::Parser::new(toks)
        .parse_program()
        .map_err(CompileError::Parse)
}

// Parse + collect declaration name spans (tooling/LSP). Separate entry so the
// normal compile path stays byte-for-byte unchanged. Returns (program, decls).
pub fn parse_program_spanned(
    src: &str,
) -> Result<(ast::Program, Vec<ast::DeclSpan>), CompileError> {
    let toks = lexer::Lexer::new(src)
        .tokenize()
        .map_err(CompileError::Lex)?;
    let mut p = parser::Parser::new_spanned(toks);
    let prog = p.parse_program().map_err(CompileError::Parse)?;
    Ok((prog, p.take_decls()))
}

pub fn compile(src: &str, base_dir: &Path, optimize: bool) -> Result<ast::Program, CompileError> {
    let mut program = parse_program(src)?;

    let mut visited = std::collections::HashSet::new();
    let mut imported = Vec::new();
    let mut aliases = std::collections::HashMap::new();
    imports::collect(
        &program,
        base_dir,
        &mut visited,
        &mut imported,
        &mut aliases,
        &builtins::is_module,
    )?;
    imports::rewrite_program(&aliases, &builtins::is_module, &mut program);
    if !imported.is_empty() {
        imported.append(&mut program);
        program = imported;
    }

    desugar::desugar_program(&mut program);
    lift::lift_program(&mut program);
    if optimize {
        opt::optimize_program(&mut program);
    }
    Ok(program)
}
