use super::rng::FastRng;

/// SQL generator for fuzzing: produces random but structurally plausible SQL.
pub struct SqlGenerator {
    types: Vec<&'static str>,
    agg_funcs: Vec<&'static str>,
    scalar_funcs: Vec<&'static str>,
    cmp_ops: Vec<&'static str>,
    arith_ops: Vec<&'static str>,
    bool_ops: Vec<&'static str>,
    tables: Vec<&'static str>,
    columns: Vec<&'static str>,
}

impl SqlGenerator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            types: vec![
                "INTEGER",
                "BIGINT",
                "TEXT",
                "BOOLEAN",
                "REAL",
                "DOUBLE PRECISION",
                "NUMERIC",
                "DATE",
                "TIMESTAMP",
                "BYTEA",
                "UUID",
            ],
            agg_funcs: vec!["count", "sum", "avg", "min", "max"],
            scalar_funcs: vec![
                "abs", "upper", "lower", "length", "trim", "coalesce", "nullif", "round", "ceil",
                "floor",
            ],
            cmp_ops: vec!["=", "<>", "<", ">", "<=", ">="],
            arith_ops: vec!["+", "-", "*", "/", "%"],
            bool_ops: vec!["AND", "OR"],
            tables: vec!["fz_t1", "fz_t2"],
            columns: vec!["id", "a", "b", "x", "y"],
        }
    }

    /// Generate a random SQL expression up to `depth` levels.
    pub fn random_expression(&self, rng: &mut FastRng, depth: u32) -> String {
        if depth == 0 || rng.chance(40) {
            return self.random_atom(rng);
        }

        let kind = rng.next_range(0, 10);
        match kind {
            0..=2 => {
                // binary arithmetic
                let op = rng.pick(&self.arith_ops);
                format!(
                    "({} {} {})",
                    self.random_expression(rng, depth - 1),
                    op,
                    self.random_expression(rng, depth - 1)
                )
            }
            3..=4 => {
                // comparison
                let op = rng.pick(&self.cmp_ops);
                format!(
                    "({} {} {})",
                    self.random_expression(rng, depth - 1),
                    op,
                    self.random_expression(rng, depth - 1)
                )
            }
            5 => {
                // boolean combinator
                let op = rng.pick(&self.bool_ops);
                format!(
                    "({} {} {})",
                    self.random_expression(rng, depth - 1),
                    op,
                    self.random_expression(rng, depth - 1)
                )
            }
            6 => {
                // function call
                let func = rng.pick(&self.scalar_funcs);
                let arg = self.random_expression(rng, depth - 1);
                match *func {
                    "coalesce" => format!(
                        "coalesce({arg}, {})",
                        self.random_expression(rng, depth - 1)
                    ),
                    "nullif" => {
                        format!("nullif({arg}, {})", self.random_expression(rng, depth - 1))
                    }
                    "round" if rng.chance(50) => format!("round({arg}, 2)"),
                    _ => format!("{func}({arg})"),
                }
            }
            7 => {
                // CASE expression
                format!(
                    "CASE WHEN {} THEN {} ELSE {} END",
                    self.random_expression(rng, depth - 1),
                    self.random_expression(rng, depth - 1),
                    self.random_expression(rng, depth - 1)
                )
            }
            8 => {
                // CAST
                let ty = rng.pick(&self.types);
                format!("CAST({} AS {ty})", self.random_expression(rng, depth - 1))
            }
            _ => {
                // IS NULL / IS NOT NULL
                if rng.chance(50) {
                    format!("({} IS NULL)", self.random_expression(rng, depth - 1))
                } else {
                    format!("({} IS NOT NULL)", self.random_expression(rng, depth - 1))
                }
            }
        }
    }

    fn random_atom(&self, rng: &mut FastRng) -> String {
        let kind = rng.next_range(0, 8);
        match kind {
            0 => rng.next_i64(-1000, 1000).to_string(),
            1 => format!("{}.{}", rng.next_i64(-100, 100), rng.next_range(0, 100)),
            2 => {
                let len = rng.next_range(0, 20) as usize;
                format!("'{}'", rng.random_alnum(len))
            }
            3 => if rng.chance(50) { "true" } else { "false" }.to_owned(),
            4 => "NULL".to_owned(),
            5 => format!("{}::integer", rng.next_i64(-1000, 1000)),
            6 => format!("'{}'::text", rng.random_alnum(5)),
            _ => rng.next_i64(0, 100).to_string(),
        }
    }

    pub fn random_identifier(&self, rng: &mut FastRng) -> String {
        rng.random_alnum(6)
    }

    /// Generate a random SELECT query using the fuzzer tables.
    pub fn random_select_query(&self, rng: &mut FastRng) -> String {
        let table = rng.pick(&self.tables);
        let mut sql = String::from("SELECT ");

        // Projection
        let proj_count = rng.next_range(1, 4);
        let projs: Vec<String> = (0..proj_count)
            .map(|_| {
                if rng.chance(30) {
                    self.random_expression(rng, 2)
                } else if rng.chance(20) {
                    let func = rng.pick(&self.agg_funcs);
                    let col = rng.pick(&self.columns);
                    if *func == "count" && rng.chance(50) {
                        "count(*)".to_owned()
                    } else {
                        format!("{func}({col})")
                    }
                } else {
                    (*rng.pick(&self.columns)).to_owned()
                }
            })
            .collect();
        sql.push_str(&projs.join(", "));

        sql.push_str(&format!(" FROM {table}"));

        // Optional JOIN
        if rng.chance(30) {
            let other = if *table == "fz_t1" { "fz_t2" } else { "fz_t1" };
            let join_type = rng.pick(&["JOIN", "LEFT JOIN", "RIGHT JOIN", "CROSS JOIN"]);
            if *join_type == "CROSS JOIN" {
                sql.push_str(&format!(" CROSS JOIN {other}"));
            } else {
                sql.push_str(&format!(" {join_type} {other} ON {table}.id = {other}.id"));
            }
        }

        // Optional WHERE
        if rng.chance(60) {
            sql.push_str(&format!(" WHERE {}", self.random_where_clause(rng)));
        }

        // Optional GROUP BY (only if we have aggregate in projection)
        if rng.chance(20) {
            let col = rng.pick(&self.columns);
            sql.push_str(&format!(" GROUP BY {col}"));

            if rng.chance(30) {
                sql.push_str(&format!(" HAVING count(*) > {}", rng.next_range(0, 5)));
            }
        }

        // Optional ORDER BY
        if rng.chance(40) {
            let col = rng.pick(&self.columns);
            let dir = if rng.chance(50) { "ASC" } else { "DESC" };
            sql.push_str(&format!(" ORDER BY {col} {dir}"));
        }

        // Optional LIMIT
        if rng.chance(50) {
            sql.push_str(&format!(" LIMIT {}", rng.next_range(1, 100)));
        }

        sql
    }

    pub fn random_column_defs(&self, rng: &mut FastRng) -> String {
        let col_count = rng.next_range(1, 6);
        let mut cols = vec!["id INTEGER PRIMARY KEY".to_owned()];
        for i in 0..col_count {
            let ty = rng.pick(&self.types);
            cols.push(format!("c{i} {ty}"));
        }
        cols.join(", ")
    }

    pub fn random_values_row(&self, rng: &mut FastRng) -> String {
        let count = rng.next_range(2, 7);
        (0..count)
            .map(|i| {
                if i == 0 {
                    // id column - integer
                    rng.next_range(0, 100000).to_string()
                } else if rng.chance(20) {
                    "NULL".to_owned()
                } else {
                    self.random_atom(rng)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn random_set_clause(&self, rng: &mut FastRng) -> String {
        let col = rng.pick(&["a", "b", "x", "y"]);
        let val = self.random_atom(rng);
        format!("{col} = {val}")
    }

    pub fn random_where_clause(&self, rng: &mut FastRng) -> String {
        let col = rng.pick(&self.columns);
        let op = rng.pick(&self.cmp_ops);
        let val = self.random_atom(rng);
        format!("{col} {op} {val}")
    }

    /// Generate SQL that stresses edge cases and may crash.
    pub fn random_crash_sql(&self, rng: &mut FastRng) -> String {
        let kind = rng.next_range(0, 20);
        match kind {
            // Deep nesting
            0 => {
                let depth = rng.next_range(5, 30) as u32;
                format!("SELECT {}", self.random_expression(rng, depth))
            }
            // Very long literal
            1 => {
                let len = rng.next_range(1000, 10000) as usize;
                format!("SELECT '{}'", rng.random_alnum(len))
            }
            // Malformed SQL
            2 => "SELECT FROM WHERE".to_owned(),
            3 => "INSERT INTO".to_owned(),
            4 => "UPDATE SET WHERE".to_owned(),
            5 => "DELETE".to_owned(),
            6 => "CREATE TABLE".to_owned(),
            7 => "DROP TABLE".to_owned(),
            // Empty string
            8 => String::new(),
            // Just semicolons
            9 => ";;;".to_owned(),
            // Unicode stress
            10 => "SELECT '\u{0000}'".to_owned(),
            11 => format!("SELECT '{}'", "\u{FFFD}".repeat(100)),
            // Huge number of columns in SELECT
            12 => {
                let n = rng.next_range(50, 200);
                let cols: Vec<String> = (0..n).map(|i| format!("{i}")).collect();
                format!("SELECT {}", cols.join(", "))
            }
            // Nested subqueries
            13 => {
                let mut sql = "SELECT 1".to_owned();
                for _ in 0..rng.next_range(3, 15) {
                    sql = format!("SELECT * FROM ({sql}) sub");
                }
                sql
            }
            // Many UNIONs
            14 => {
                let n = rng.next_range(5, 50);
                let parts: Vec<String> = (0..n).map(|i| format!("SELECT {i}")).collect();
                parts.join(" UNION ALL ")
            }
            // Division edge cases
            15 => "SELECT 1/0, 0/0, -1/0".to_owned(),
            // Overflow
            16 => "SELECT 9223372036854775807::bigint + 9223372036854775807::bigint".to_owned(),
            // Type mismatch
            17 => "SELECT 'text' + 42".to_owned(),
            // Random binary expressions
            18 => format!("SELECT {}", self.random_expression(rng, 5)),
            // Comment edge cases
            _ => "SELECT /* unterminated comment".to_owned(),
        }
    }
}
