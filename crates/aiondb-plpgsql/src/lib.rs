//! PL/pgSQL tokenizer, parser and AST for AionDB.
//!
//! Runtime execution is driven by the engine via the [`Executor`] trait
//! defined in [`runtime`], which lets this crate stay free of engine-specific
//! types while still carrying every construct needed to execute compiled
//! PL/pgSQL programs end-to-end.

#![allow(
    clippy::doc_markdown,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::return_self_not_must_use,
    clippy::single_match_else,
    clippy::too_many_lines,
    clippy::unnecessary_wraps
)]

pub mod ast;
pub mod interpreter;
pub mod parser;
pub mod runtime;
pub mod tokenizer;

pub use ast::{
    Block, CaseArm, CaseStmt, CursorDecl, ExceptionHandler, ForLoopKind, IfBranch, RaiseLevel,
    RaiseOption, ReturnKind, Stmt, VarDecl,
};
pub use interpreter::{Flow, Interpreter};
pub use parser::{parse_block, parse_function_body, ParseError};
pub use runtime::{Executor, Frame, SqlExecution, VariableBindings};
pub use tokenizer::{tokenize, Token, TokenKind, TokenizeError};
