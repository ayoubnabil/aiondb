---
title: aiondb-plpgsql
order: 21
---

# aiondb-plpgsql

PL/pgSQL tokenizer, parser, AST and tree-walking interpreter. Runtime execution is driven by the engine through the `Executor` trait defined in `runtime`, which lets this crate stay free of engine-specific types while still carrying every construct needed to execute compiled PL/pgSQL programs end-to-end.

## cargo

```toml
[dependencies]
aiondb-plpgsql = { path = "../aiondb-plpgsql" }
```

## modules

| module | purpose |
|---|---|
| `tokenizer` | Lex PL/pgSQL source into `Token` / `TokenKind`. |
| `parser` | Build the AST: `parse_block`, `parse_function_body`. |
| `ast` | Typed AST (`Block`, `Stmt`, `VarDecl`, ...). |
| `runtime` | Engine-side glue: `Executor` trait, `Frame`, `VariableBindings`, `SqlExecution`. |
| `interpreter` | Tree-walking evaluator (`Interpreter`, `Flow`). |

## key types

- `Block` - a parsed PL/pgSQL block (declarations + body).
- `Stmt` - statement variants used inside a block (assignment, `IF`, `CASE`, `LOOP`, `FOR`, `WHILE`, `RETURN`, `RAISE`, embedded SQL, ...).
- `VarDecl`, `CursorDecl` - declared variables and cursors.
- `IfBranch`, `CaseStmt`, `CaseArm`, `ForLoopKind`, `ExceptionHandler`, `RaiseLevel`, `RaiseOption`, `ReturnKind` - sub-AST nodes.
- `Token`, `TokenKind`, `TokenizeError` - tokenizer output.
- `ParseError` - parser error type returned by `parse_block` / `parse_function_body`.
- `Executor` - trait the engine implements so the interpreter can run SQL, fetch cursors, signal notices, etc.
- `Frame`, `VariableBindings`, `SqlExecution` - runtime state passed to the interpreter.
- `Interpreter` - tree walker that runs a `Block` against an `Executor`.
- `Flow` - control-flow value returned by `Interpreter::run` (normal, return, exit, continue).

## entry points

| function | role |
|---|---|
| `tokenize(src)` | Lex source text into tokens. |
| `parse_block(src)` | Parse a `BEGIN ... END` block. |
| `parse_function_body(src)` | Parse a top-level function body. |
| `Interpreter::new(exec)` | Build an interpreter bound to an `Executor`. |
| `Interpreter::run(&block)` | Evaluate the block and return a `Flow`. |

## example

```rust
use aiondb_plpgsql::{parse_block, Block, Stmt};

let src = "
    BEGIN
        x := 1;
        IF x > 0 THEN
            RETURN x;
        END IF;
    END;
";

let block: Block = parse_block(src).expect("parse plpgsql");
assert!(!block.body.is_empty());
let _: &Stmt = &block.body[0];
```

To actually run a parsed block, an engine-side type implements the `Executor` trait (so the interpreter can issue SQL, fetch cursors, ...) and is passed to `Interpreter::new`.
