---
title: aiondb-parser
order: 20
---

# aiondb-parser

Lexer, token validator and recursive-descent parser that turns SQL (and Cypher) text into the AST consumed by the planner. The crate enforces hard limits on input size, token count, identifier length and bracket nesting depth before any recursive parsing begins, so the planner never sees inputs that could exhaust the stack or balloon allocations.

## cargo

```toml
[dependencies]
aiondb-parser = { path = "../aiondb-parser" }
```

## modules

| module | purpose |
|---|---|
| `ast` | SQL statement AST (`Statement`, `SelectStatement`, expressions, DDL/DML nodes). |
| `cypher_ast` | Cypher AST (`CypherStatement`, `CypherClause`, patterns). |
| `identifier` | Identifier classification helpers. |
| `keywords` | Reserved-word table (`Keyword`). |
| `lexer` | `lex_sql` and the raw lexer driver. |
| `tokens` | `Token` and `TokenKind`. |
| `span` | `Span` source-position type. |
| `parser_acl` | `GRANT` / `REVOKE` parsing. |
| `parser_async` | `LISTEN` / `NOTIFY` / `UNLISTEN`. |
| `parser_backup` | Backup-related statements. |
| `parser_comment` | `COMMENT ON ...`. |
| `parser_copy` | `COPY` statement. |
| `parser_cypher` | Cypher entrypoint. |
| `parser_ddl` / `parser_ddl_ext` | `CREATE` / `ALTER` / `DROP` for tables, indexes, sequences, views, triggers, schemas, roles, extensions. |
| `parser_dml` | `INSERT`, `UPDATE`, `DELETE`, `MERGE`. |
| `parser_expr` | Expression precedence climber. |
| `parser_func` | Function-call and aggregate parsing. |
| `parser_lock` | `LOCK TABLE`. |
| `parser_owned` | `DROP OWNED` / `REASSIGN OWNED`. |
| `parser_security_label` | `SECURITY LABEL ...`. |
| `parser_select` | `SELECT`, set operations, CTEs. |
| `parser_session` | `SET` / `RESET` / `SHOW` / `DISCARD`. |
| `parser_tx` | Transaction-control statements. |
| `parser_types` | Type-name parsing. |

## entry points

| function | input | output |
|---|---|---|
| `parse_sql(sql: &str)` | full SQL text | `DbResult<Vec<Statement>>` |
| `parse_prepared_statement(sql: &str)` | a single statement | `DbResult<Statement>` |
| `parse_expression(sql: &str)` | a single expression | `DbResult<Expr>` |
| `parse_cypher(sql: &str)` | Cypher text | `DbResult<CypherStatement>` |

Every entry point first runs `lex_and_validate`, which rejects oversized inputs and unbalanced brackets in a single O(n) pass.

## key types

- `Statement` - top-level SQL AST node, covering DDL, DML, transactions, sessions and Cypher.
- `SelectStatement`, `InsertStatement`, `UpdateStatement`, `DeleteStatement`, `MergeStatement` - DML variants.
- `CreateTableStatement`, `CreateIndexStatement`, `CreateSequenceStatement`, `CreateViewStatement`, `CreateTriggerStatement`, `CreateFunctionStatement`, `CreateExtensionStatement`, `CreateRoleStatement`, `CreateSchemaStatement`, `CreateNodeLabelStatement`, `CreateEdgeLabelStatement` - DDL variants.
- `Expr` - expression tree with `Literal`, `BinaryOperator`, `UnaryOperator`.
- `JoinClause`, `JoinType`, `OrderByItem`, `SelectItem`, `GroupByItem`, `GroupBySet`, `OnConflict`.
- `CypherStatement` and clause types (`CypherMatchClause`, `CypherCreateClause`, `CypherMergeClause`, `CypherReturnClause`, `CypherWithClause`, `CypherUnwindClause`, `CypherForeachClause`, `CypherDeleteClause`, `CypherSetClause`, `CypherRemoveClause`, `CypherCallClause`).
- `Span` - byte range pointing back into the source string.
- `Token`, `TokenKind`, `Keyword` - lexer output.

## limits

The crate publishes its own resource ceilings and surfaces them through `DbError::program_limit` when violated. The defaults are:

| name | value | meaning |
|---|---|---|
| `MAX_SQL_INPUT_BYTES` | 64 MiB | rejects oversized SQL text up front. |
| `MAX_TOKEN_COUNT` | 500000 | upper bound on tokens per statement. |
| `MAX_IDENTIFIER_TOKEN_LEN` | 1024 | per-identifier byte length. |
| `DEFAULT_MAX_NESTING_DEPTH` | 32 (debug) / 48 (release) | parenthesis and bracket nesting. |

Nesting depth can be overridden through `AIONDB_PARSER_MAX_NESTING_DEPTH`, clamped to `[8, 128]`.

## example

```rust
use aiondb_parser::{parse_sql, parse_expression, Statement};

let stmts = parse_sql("select 1 + 2; select 'hi'").expect("parse");
assert_eq!(stmts.len(), 2);
assert!(matches!(stmts[0], Statement::Select(_)));

let expr = parse_expression("a + b * 2").expect("expr");
let _ = expr;
```
