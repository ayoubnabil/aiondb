use std::path::PathBuf;

fn main() {
    let sql_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sql");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&sql_dir)
        .expect("cannot read sql/")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    files.sort();

    let mut total_paren_errors = 0usize;

    for path in &files {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let stmts = split_sql_statements(&content);

        for stmt in &stmts {
            match aiondb_parser::parse_sql(stmt) {
                Ok(_) => {}
                Err(e) => {
                    let msg = format!("{}", e);
                    if msg.contains("expected ')'") {
                        let short = stmt.chars().take(150).collect::<String>().replace('\n', " ");
                        eprintln!("ERR|{}|{}|{}", name, msg, short);
                        total_paren_errors += 1;
                    }
                }
            }
        }
    }

    eprintln!("TOTAL_PAREN_ERRORS: {}", total_paren_errors);
}

fn split_sql_statements(content: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_dollar_quote = false;
    let mut dollar_tag = String::new();
    let mut in_single_quote = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_copy_data = false;
    let mut prev_char = '\0';

    for line in content.lines() {
        let trimmed = line.trim();

        if in_copy_data {
            if trimmed == "\\." {
                in_copy_data = false;
            }
            continue;
        }

        if !in_dollar_quote && !in_single_quote && !in_block_comment {
            if trimmed.starts_with('\\') {
                continue;
            }
        }

        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();
        let mut i = 0;

        while i < len {
            let ch = chars[i];

            if in_line_comment {
                i += 1;
                continue;
            }

            if in_block_comment {
                if ch == '*' && i + 1 < len && chars[i + 1] == '/' {
                    in_block_comment = false;
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }

            if !in_single_quote
                && !in_dollar_quote
                && ch == '-'
                && i + 1 < len
                && chars[i + 1] == '-'
            {
                in_line_comment = true;
                i += 2;
                continue;
            }

            if !in_single_quote
                && !in_dollar_quote
                && ch == '/'
                && i + 1 < len
                && chars[i + 1] == '*'
            {
                in_block_comment = true;
                i += 2;
                continue;
            }

            if in_dollar_quote {
                current.push(ch);
                if ch == '$' {
                    let remaining: String = chars[i..].iter().collect();
                    if remaining.starts_with(&dollar_tag) {
                        for c in dollar_tag.chars().skip(1) {
                            i += 1;
                            current.push(c);
                        }
                        in_dollar_quote = false;
                        dollar_tag.clear();
                    }
                }
                i += 1;
                continue;
            }

            if in_single_quote {
                current.push(ch);
                if ch == '\'' && prev_char != '\'' {
                    if i + 1 < len && chars[i + 1] == '\'' {
                        current.push('\'');
                        i += 2;
                        prev_char = '\0';
                        continue;
                    }
                    in_single_quote = false;
                }
                prev_char = ch;
                i += 1;
                continue;
            }

            if ch == '$' {
                let mut tag = String::from("$");
                let mut j = i + 1;
                while j < len && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    tag.push(chars[j]);
                    j += 1;
                }
                if j < len && chars[j] == '$' {
                    tag.push('$');
                    in_dollar_quote = true;
                    dollar_tag = tag.clone();
                    for c in tag.chars() {
                        current.push(c);
                    }
                    i = j + 1;
                    continue;
                }
            }

            if ch == '\'' {
                in_single_quote = true;
                current.push(ch);
                i += 1;
                continue;
            }

            if ch == ';' {
                let stmt = current.trim().to_string();
                if !stmt.is_empty() {
                    let upper = stmt.to_ascii_uppercase();
                    if upper.contains("FROM STDIN") && upper.starts_with("COPY") {
                        in_copy_data = true;
                    }
                    statements.push(stmt);
                }
                current.clear();
                i += 1;
                continue;
            }

            current.push(ch);
            prev_char = ch;
            i += 1;
        }

        in_line_comment = false;
        if !current.is_empty() {
            current.push(' ');
        }
    }

    let stmt = current.trim().to_string();
    if !stmt.is_empty() {
        statements.push(stmt);
    }

    statements
}
