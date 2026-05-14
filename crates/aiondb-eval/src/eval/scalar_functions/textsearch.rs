#![allow(clippy::doc_markdown)]

use aiondb_core::{DbResult, Value};

use super::value_convert::usize_to_f32;
use super::{expect_args, expect_at_least_args};

// =====================================================================
// English stop words (from PostgreSQL's english.stop)
// =====================================================================

const ENGLISH_STOP_WORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "am",
    "an",
    "and",
    "any",
    "are",
    "aren't",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "can't",
    "cannot",
    "could",
    "couldn't",
    "did",
    "didn't",
    "do",
    "does",
    "doesn't",
    "doing",
    "don't",
    "down",
    "during",
    "each",
    "few",
    "for",
    "from",
    "further",
    "had",
    "hadn't",
    "has",
    "hasn't",
    "have",
    "haven't",
    "having",
    "he",
    "he'd",
    "he'll",
    "he's",
    "her",
    "here",
    "here's",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "how's",
    "i",
    "i'd",
    "i'll",
    "i'm",
    "i've",
    "if",
    "in",
    "into",
    "is",
    "isn't",
    "it",
    "it's",
    "its",
    "itself",
    "let's",
    "me",
    "more",
    "most",
    "mustn't",
    "my",
    "myself",
    "no",
    "nor",
    "not",
    "of",
    "off",
    "on",
    "once",
    "only",
    "or",
    "other",
    "ought",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "same",
    "shan't",
    "she",
    "she'd",
    "she'll",
    "she's",
    "should",
    "shouldn't",
    "so",
    "some",
    "such",
    "than",
    "that",
    "that's",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "there's",
    "these",
    "they",
    "they'd",
    "they'll",
    "they're",
    "they've",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "very",
    "was",
    "wasn't",
    "we",
    "we'd",
    "we'll",
    "we're",
    "we've",
    "were",
    "weren't",
    "what",
    "what's",
    "when",
    "when's",
    "where",
    "where's",
    "which",
    "while",
    "who",
    "who's",
    "whom",
    "why",
    "why's",
    "will",
    "with",
    "won't",
    "would",
    "wouldn't",
    "you",
    "you'd",
    "you'll",
    "you're",
    "you've",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

fn is_english_stop_word(word: &str) -> bool {
    ENGLISH_STOP_WORDS.binary_search(&word).is_ok()
}

/// Determine whether a configuration name indicates English processing.
fn is_english_config(config: &str) -> bool {
    let lower = config.to_lowercase();
    lower == "english" || lower == "pg_catalog.english"
}

/// Determine text search configuration from arguments.
/// Returns `true` for English config, `false` for simple/other.
fn resolve_config(args: &[Value]) -> bool {
    if args.len() >= 2 {
        let config = value_to_string(&args[0]);
        is_english_config(&config)
    } else {
        // Default is 'english' in PostgreSQL
        true
    }
}

// =====================================================================
// Public API functions
// =====================================================================

/// Convert text into a tsvector representation.
///
/// A tsvector is a sorted list of distinct lexemes (normalized words) with
/// optional positional information. This implementation produces a
/// PostgreSQL-compatible textual representation:
///   'word1':1 'word2':2 ...
///
/// When called with two arguments the first is the configuration name.
/// Supported configs: 'simple' (no stop words, no stemming) and
/// 'english' (stop words + basic stemming).
pub fn eval_to_tsvector(args: &[Value]) -> DbResult<Value> {
    let (text, use_english) = extract_text_and_config(args, "to_tsvector")?;
    if text.is_empty() {
        return Ok(Value::Text(String::new()));
    }
    let lexemes = tokenize_to_lexemes(&text, use_english);
    Ok(Value::Text(format_tsvector(&lexemes)))
}

/// Convert text into a tsquery using the PostgreSQL query syntax.
///
/// Accepted operators inside the query string:
///   `&` (AND), `|` (OR), `!` (NOT), `<->` (phrase/FOLLOWED BY)
/// Terms are quoted with single quotes: `'word1' & 'word2'`.
/// Unquoted bare words are also accepted.
pub fn eval_to_tsquery(args: &[Value]) -> DbResult<Value> {
    let (text, use_english) = extract_text_and_config(args, "to_tsquery")?;
    let normalized = normalize_tsquery(&text, use_english);
    Ok(Value::Text(normalized))
}

/// Convert plain text into a tsquery. All words are ANDed together.
pub fn eval_plainto_tsquery(args: &[Value]) -> DbResult<Value> {
    let (text, use_english) = extract_text_and_config(args, "plainto_tsquery")?;
    let words = tokenize_and_filter(&text, use_english);
    if words.is_empty() {
        return Ok(Value::Text(String::new()));
    }
    Ok(Value::Text(build_tsquery_join(&words, " & ")))
}

/// Convert text into a phrase tsquery. Words are connected by `<->` (FOLLOWED BY).
pub fn eval_phraseto_tsquery(args: &[Value]) -> DbResult<Value> {
    let (text, use_english) = extract_text_and_config(args, "phraseto_tsquery")?;
    let words = tokenize_and_filter(&text, use_english);
    if words.is_empty() {
        return Ok(Value::Text(String::new()));
    }
    Ok(Value::Text(build_tsquery_join(&words, " <-> ")))
}

/// Join `'word1' SEP 'word2' SEP ...` into a single buffer, replacing
/// the previous `words.iter().map(|w| format!("'{w}'")).collect().join`
/// chain that allocated one String per word + a Vec + a joined String
/// per call.
#[inline]
fn build_tsquery_join(words: &[String], separator: &str) -> String {
    let mut out = String::with_capacity(words.len() * 8);
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            out.push_str(separator);
        }
        out.push('\'');
        out.push_str(w);
        out.push('\'');
    }
    out
}

/// Convert web-search syntax into a tsquery.
///
/// Rules:
///   - unquoted words are ANDed
///   - `"quoted phrase"` becomes `word1 <-> word2`
///   - `OR` between terms produces `|`
///   - `-word` produces `!word`
pub fn eval_websearch_to_tsquery(args: &[Value]) -> DbResult<Value> {
    let (text, use_english) = extract_text_and_config(args, "websearch_to_tsquery")?;
    let query = parse_websearch(&text, use_english);
    Ok(Value::Text(query))
}

/// Highlight matching terms in text by wrapping them in `<b>...</b>`.
///
/// `ts_headline(text, tsquery[, options])` or
/// `ts_headline(config, text, tsquery[, options])`.
pub fn eval_ts_headline(args: &[Value]) -> DbResult<Value> {
    expect_at_least_args(args, 2, "ts_headline")?;
    // Determine which arg is the document and which is the query.
    let (doc_text, query_text) = if args.len() >= 3 {
        let arg2 = value_to_string(&args[2]);
        if arg2.contains('&')
            || arg2.contains('|')
            || arg2.contains('!')
            || arg2.contains('\'')
            || arg2.contains("<->")
        {
            (value_to_string(&args[1]), arg2)
        } else {
            (value_to_string(&args[0]), value_to_string(&args[1]))
        }
    } else {
        (value_to_string(&args[0]), value_to_string(&args[1]))
    };

    let query_words = extract_query_words(&query_text);
    if query_words.is_empty() {
        return Ok(Value::Text(doc_text));
    }

    let highlighted = highlight_text(&doc_text, &query_words);
    Ok(Value::Text(highlighted))
}

/// Lexize a word using a text search dictionary.
///
/// `ts_lexize(regdictionary, text)` returns `text[]`.
pub fn eval_ts_lexize(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "ts_lexize")?;
    let dict = value_to_string(&args[0]);
    let word = value_to_string(&args[1]);
    if word.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let use_english = is_english_config(&dict) || dict.eq_ignore_ascii_case("english_stem");
    let lexeme = if use_english {
        english_stem(&word.to_lowercase())
    } else {
        word.to_lowercase()
    };
    Ok(Value::Array(vec![Value::Text(lexeme)]))
}

/// ts_rank: compute a relevance score.
///
/// Returns a Real (float4) value matching PostgreSQL's return type.
pub fn eval_ts_rank(args: &[Value]) -> DbResult<Value> {
    expect_at_least_args(args, 2, "ts_rank")?;
    let (tsvec_str, tsq_str) = if args.len() >= 3 {
        (
            value_to_string(&args[args.len() - 2]),
            value_to_string(&args[args.len() - 1]),
        )
    } else {
        (value_to_string(&args[0]), value_to_string(&args[1]))
    };

    let tsvec_words = extract_tsvector_words(&tsvec_str);
    let query_words = extract_query_words(&tsq_str);
    if tsvec_words.is_empty() || query_words.is_empty() {
        return Ok(Value::Real(0.0));
    }
    let matching = query_words
        .iter()
        .filter(|qw| tsvec_words.iter().any(|tw| tw == *qw))
        .count();
    // PostgreSQL uses a more complex formula, but this is a reasonable approximation
    let score = usize_to_f32(matching) / usize_to_f32(query_words.len().max(1));
    // Scale to typical PG ts_rank range (0.0 to ~0.6)
    let scaled = score * 0.6;
    // Round to 8 significant digits like PG
    Ok(Value::Real(scaled))
}

/// ts_rank_cd: cover density ranking. For basic compat, same as ts_rank.
pub fn eval_ts_rank_cd(args: &[Value]) -> DbResult<Value> {
    eval_ts_rank(args)
}

pub fn eval_full_text_match_rank(
    config: &str,
    document: &str,
    tsquery: &str,
) -> DbResult<Option<f32>> {
    if document.is_empty() || tsquery.trim().is_empty() {
        return Ok(None);
    }
    let use_english = is_english_config(config);
    let lexemes = tokenize_to_lexemes(document, use_english);
    if lexemes.is_empty() {
        return Ok(None);
    }
    let mut words = Vec::with_capacity(lexemes.len());
    let mut positions = std::collections::HashMap::with_capacity(lexemes.len());
    for (word, word_positions) in lexemes {
        positions.insert(word.clone(), word_positions);
        words.push(word);
    }
    if !evaluate_tsquery(tsquery, &words, &positions) {
        return Ok(None);
    }
    let query_words = extract_query_words(tsquery);
    if query_words.is_empty() {
        return Ok(None);
    }
    let matching = query_words
        .iter()
        .filter(|qw| words.iter().any(|word| word == *qw))
        .count();
    let score = usize_to_f32(matching) / usize_to_f32(query_words.len().max(1));
    Ok(Some(score * 0.6))
}

/// `tsvector @@ tsquery` - evaluate a tsquery against a tsvector.
///
/// Handles AND (&), OR (|), NOT (!) operators and phrase matching (<->).
pub fn eval_ts_match(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "ts_match")?;
    if args[0].is_null() || args[1].is_null() {
        return Ok(Value::Null);
    }

    let tsvec_str = value_to_string(&args[0]);
    let tsquery_str = value_to_string(&args[1]);

    if tsquery_str.trim().is_empty() {
        return Ok(Value::Boolean(false));
    }

    let tsvec_words = extract_tsvector_words(&tsvec_str);
    let tsvec_positions = extract_tsvector_positions(&tsvec_str);
    let result = evaluate_tsquery(&tsquery_str, &tsvec_words, &tsvec_positions);
    Ok(Value::Boolean(result))
}

// =====================================================================
// Internal helpers
// =====================================================================

/// Extract the text argument and config flag.
fn extract_text_and_config(args: &[Value], func_name: &str) -> DbResult<(String, bool)> {
    expect_at_least_args(args, 1, func_name)?;
    if args.iter().any(Value::is_null) {
        return Ok((String::new(), false));
    }
    let use_english = resolve_config(args);
    let text_arg = if args.len() >= 2 { &args[1] } else { &args[0] };
    Ok((value_to_string(text_arg), use_english))
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Text(s) => s.clone(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Boolean(b) => (if *b { "true" } else { "false" }).to_string(),
        other => format!("{other}"),
    }
}

/// Tokenize text into lowercased words.
fn tokenize_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '\'')
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Tokenize and filter: lowercase, optionally remove stop words and stem.
fn tokenize_and_filter(text: &str, use_english: bool) -> Vec<String> {
    let words = tokenize_words(text);
    words
        .into_iter()
        .filter(|w| !use_english || !is_english_stop_word(w))
        .map(|w| if use_english { english_stem(&w) } else { w })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Tokenize and lexize text into a deduplicated, sorted list of lexemes.
pub(super) fn tokenize_to_lexemes(text: &str, use_english: bool) -> Vec<(String, Vec<usize>)> {
    let words = tokenize_words(text);
    let mut lexeme_map: std::collections::BTreeMap<String, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, word) in words.iter().enumerate() {
        if use_english && is_english_stop_word(word) {
            continue;
        }
        let lexeme = if use_english {
            english_stem(word)
        } else {
            word.clone()
        };
        if !lexeme.is_empty() {
            lexeme_map.entry(lexeme).or_default().push(i + 1); // 1-based positions
        }
    }
    lexeme_map.into_iter().collect()
}

/// Format lexemes into PostgreSQL tsvector text representation.
pub(super) fn format_tsvector(lexemes: &[(String, Vec<usize>)]) -> String {
    use std::fmt::Write as _;
    // Build the entire `'lexeme1':p1,p2 'lexeme2':p3` output in a
    // single buffer. The previous nested-`map(...).collect::<Vec<_>>()`
    // chains paid one heap allocation per position, one per lexeme,
    // and one per outer/inner join - for an N-lexeme tsvector with
    // average M positions that's N*(M+2)+(N+1) surface allocations.
    // A capacity hint of 16 bytes per lexeme covers the common token
    // shape and grows naturally for longer ones.
    let mut out = String::with_capacity(lexemes.len() * 16);
    for (lex_idx, (lexeme, positions)) in lexemes.iter().enumerate() {
        if lex_idx > 0 {
            out.push(' ');
        }
        out.push('\'');
        out.push_str(lexeme);
        out.push_str("':");
        for (pos_idx, p) in positions.iter().enumerate() {
            if pos_idx > 0 {
                out.push(',');
            }
            let _ = write!(out, "{p}");
        }
    }
    out
}

/// Basic English stemmer: lowercase and strip common suffixes.
///
/// This implements a simplified Porter-like stemming algorithm suitable
/// for PostgreSQL compatibility. It handles the most common English
/// suffixes to normalize words to their root forms.
fn english_stem(word: &str) -> String {
    let w = word.to_lowercase();
    if w.len() <= 2 {
        return w;
    }

    // Step 1: plural/verb forms
    if let Some(base) = w.strip_suffix("ies") {
        if base.len() >= 2 {
            return format!("{base}i");
        }
    }
    if let Some(base) = w.strip_suffix("sses") {
        if !base.is_empty() {
            return format!("{base}ss");
        }
    }
    if let Some(base) = w.strip_suffix("ness") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ment") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ing") {
        if base.len() >= 3 {
            // running -> run (double consonant)
            let bytes = base.as_bytes();
            if bytes.len() >= 2 && bytes[bytes.len() - 1] == bytes[bytes.len() - 2] {
                return base[..base.len() - 1].to_string();
            }
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("tion") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ation") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ful") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ous") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ive") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ly") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ed") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("er") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("es") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix('s') {
        if base.len() >= 3 && !base.ends_with('s') {
            return base.to_string();
        }
    }

    w
}

/// Normalize a tsquery string: ensure terms are quoted, operators are valid.
fn normalize_tsquery(input: &str, use_english: bool) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // If already contains single-quoted terms, normalize minimally
    if trimmed.contains('\'') {
        return trimmed.to_string();
    }

    // Bare words: split by whitespace and operators, quote terms
    let mut result = String::new();
    let mut chars = trimmed.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c == '&' || c == '|' || c == '!' {
            result.push(' ');
            result.push(c);
            result.push(' ');
            chars.next();
        } else if c == '<' {
            let mut op = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == '>' {
                    op.push(nc);
                    chars.next();
                    break;
                }
                op.push(nc);
                chars.next();
            }
            // Stream the spaced operator directly instead of allocating
            // a transient `format!(" {op} ")` String.
            result.push(' ');
            result.push_str(&op);
            result.push(' ');
        } else if c == '(' || c == ')' {
            result.push(' ');
            result.push(c);
            result.push(' ');
            chars.next();
        } else if c.is_whitespace() {
            chars.next();
        } else {
            let mut word = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_alphanumeric() || nc == '_' {
                    word.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            // Ensure forward progress on unexpected punctuation (e.g. ":")
            // so normalization cannot loop forever on malformed or decorated terms.
            if word.is_empty() {
                chars.next();
                continue;
            }

            // Preserve optional tsquery suffix modifiers such as :A, :b or :AB*.
            let mut suffix = String::new();
            if chars.peek() == Some(&':') {
                suffix.push(':');
                chars.next();
                while let Some(&mc) = chars.peek() {
                    if mc.is_whitespace()
                        || mc == '&'
                        || mc == '|'
                        || mc == '!'
                        || mc == '<'
                        || mc == '>'
                        || mc == '('
                        || mc == ')'
                    {
                        break;
                    }
                    suffix.push(mc);
                    chars.next();
                }
            }
            while chars.peek() == Some(&'*') {
                suffix.push('*');
                chars.next();
            }

            let lower = word.to_lowercase();
            if use_english && is_english_stop_word(&lower) {
                // Skip stop words in tsquery for English config.
                continue;
            }
            let stemmed = if use_english {
                english_stem(&lower)
            } else {
                lower
            };
            // Stream `'<stemmed>'<suffix>` directly into `result`
            // instead of building a transient String via format!.
            result.push('\'');
            result.push_str(&stemmed);
            result.push('\'');
            result.push_str(&suffix);
        }
    }

    result.trim().to_string()
}

/// Extract individual query words from a tsquery string (ignoring operators).
fn extract_query_words(query: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut in_quote = false;
    let mut current = String::new();

    for c in query.chars() {
        if c == '\'' {
            if in_quote {
                if !current.is_empty() {
                    words.push(current.to_lowercase());
                    current.clear();
                }
                in_quote = false;
            } else {
                in_quote = true;
            }
        } else if in_quote || c.is_alphanumeric() || c == '_' {
            current.push(c);
        } else if !current.is_empty() {
            let lower = current.to_lowercase();
            if lower != "and" && lower != "or" && lower != "not" {
                words.push(lower);
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        let lower = current.to_lowercase();
        if lower != "and" && lower != "or" && lower != "not" {
            words.push(lower);
        }
    }
    words
}

/// Extract words from a tsvector string representation.
fn extract_tsvector_words(tsvec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut in_quote = false;
    let mut current = String::new();

    for c in tsvec.chars() {
        if c == '\'' {
            if in_quote {
                if !current.is_empty() {
                    words.push(current.clone());
                    current.clear();
                }
                in_quote = false;
            } else {
                in_quote = true;
            }
        } else if in_quote {
            current.push(c);
        }
    }
    words
}

/// Extract word positions from a tsvector string.
/// Returns a map of word -> sorted positions.
fn extract_tsvector_positions(tsvec: &str) -> std::collections::HashMap<String, Vec<usize>> {
    let mut map = std::collections::HashMap::new();
    // Parse format: 'word':1,2,3 'word2':4
    let mut chars = tsvec.chars().peekable();

    while chars.peek().is_some() {
        // Skip whitespace
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek() != Some(&'\'') {
            chars.next();
            continue;
        }
        chars.next(); // consume opening '
        let mut word = String::new();
        while let Some(&c) = chars.peek() {
            if c == '\'' {
                chars.next();
                break;
            }
            word.push(c);
            chars.next();
        }
        // Now parse optional :positions
        let mut positions = Vec::new();
        if chars.peek() == Some(&':') {
            chars.next(); // consume :
            let mut num_str = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    num_str.push(c);
                    chars.next();
                } else if c == ',' {
                    if let Ok(n) = num_str.parse::<usize>() {
                        positions.push(n);
                    }
                    num_str.clear();
                    chars.next();
                } else {
                    break;
                }
            }
            if let Ok(n) = num_str.parse::<usize>() {
                positions.push(n);
            }
        }
        if !word.is_empty() {
            map.insert(word, positions);
        }
    }
    map
}

// =====================================================================
// Tsquery evaluation engine
// =====================================================================

/// A parsed tsquery node.
#[derive(Debug)]
enum TsQueryNode {
    Term(String),
    And(Box<TsQueryNode>, Box<TsQueryNode>),
    Or(Box<TsQueryNode>, Box<TsQueryNode>),
    Not(Box<TsQueryNode>),
    Phrase(Vec<String>), // words that must appear consecutively
}

/// Parse a tsquery string into a tree for evaluation.
fn parse_tsquery_expr(input: &str) -> Option<TsQueryNode> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let tokens = tokenize_tsquery(trimmed);
    if tokens.is_empty() {
        return None;
    }
    let mut pos = 0;
    parse_or_expr(&tokens, &mut pos)
}

#[derive(Debug, Clone)]
enum TsQueryToken {
    Term(String),
    And,
    Or,
    Not,
    Phrase, // <->
    LParen,
    RParen,
}

fn tokenize_tsquery(input: &str) -> Vec<TsQueryToken> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '\'' {
            chars.next(); // consume '
            let mut word = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == '\'' {
                    chars.next();
                    break;
                }
                word.push(nc);
                chars.next();
            }
            if !word.is_empty() {
                tokens.push(TsQueryToken::Term(word.to_lowercase()));
            }
        } else if c == '&' {
            tokens.push(TsQueryToken::And);
            chars.next();
        } else if c == '|' {
            tokens.push(TsQueryToken::Or);
            chars.next();
        } else if c == '!' {
            tokens.push(TsQueryToken::Not);
            chars.next();
        } else if c == '<' {
            // Check for <->
            chars.next();
            if chars.peek() == Some(&'-') {
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
            }
            tokens.push(TsQueryToken::Phrase);
        } else if c == '(' {
            tokens.push(TsQueryToken::LParen);
            chars.next();
        } else if c == ')' {
            tokens.push(TsQueryToken::RParen);
            chars.next();
        } else if c.is_alphanumeric() || c == '_' {
            let mut word = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_alphanumeric() || nc == '_' {
                    word.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(TsQueryToken::Term(word.to_lowercase()));
        } else {
            chars.next();
        }
    }
    tokens
}

fn parse_or_expr(tokens: &[TsQueryToken], pos: &mut usize) -> Option<TsQueryNode> {
    let mut left = parse_and_expr(tokens, pos)?;
    while *pos < tokens.len() {
        if matches!(tokens[*pos], TsQueryToken::Or) {
            *pos += 1;
            let right = parse_and_expr(tokens, pos)?;
            left = TsQueryNode::Or(Box::new(left), Box::new(right));
        } else {
            break;
        }
    }
    Some(left)
}

fn parse_and_expr(tokens: &[TsQueryToken], pos: &mut usize) -> Option<TsQueryNode> {
    let mut left = parse_phrase_expr(tokens, pos)?;
    while *pos < tokens.len() {
        if matches!(tokens[*pos], TsQueryToken::And) {
            *pos += 1;
            let right = parse_phrase_expr(tokens, pos)?;
            left = TsQueryNode::And(Box::new(left), Box::new(right));
        } else {
            break;
        }
    }
    Some(left)
}

fn parse_phrase_expr(tokens: &[TsQueryToken], pos: &mut usize) -> Option<TsQueryNode> {
    let first = parse_unary_expr(tokens, pos)?;
    // Collect phrase terms connected by <->
    let mut phrase_terms = Vec::new();
    if let TsQueryNode::Term(ref w) = first {
        phrase_terms.push(w.clone());
    }
    let mut has_phrase = false;
    while *pos < tokens.len() && matches!(tokens[*pos], TsQueryToken::Phrase) {
        has_phrase = true;
        *pos += 1;
        let next = parse_unary_expr(tokens, pos)?;
        if let TsQueryNode::Term(ref w) = next {
            phrase_terms.push(w.clone());
        }
    }
    if has_phrase && phrase_terms.len() >= 2 {
        Some(TsQueryNode::Phrase(phrase_terms))
    } else {
        Some(first)
    }
}

fn parse_unary_expr(tokens: &[TsQueryToken], pos: &mut usize) -> Option<TsQueryNode> {
    if *pos >= tokens.len() {
        return None;
    }
    match &tokens[*pos] {
        TsQueryToken::Not => {
            *pos += 1;
            let inner = parse_unary_expr(tokens, pos)?;
            Some(TsQueryNode::Not(Box::new(inner)))
        }
        TsQueryToken::LParen => {
            *pos += 1;
            let inner = parse_or_expr(tokens, pos)?;
            if *pos < tokens.len() && matches!(tokens[*pos], TsQueryToken::RParen) {
                *pos += 1;
            }
            Some(inner)
        }
        TsQueryToken::Term(w) => {
            let node = TsQueryNode::Term(w.clone());
            *pos += 1;
            Some(node)
        }
        _ => {
            *pos += 1;
            parse_unary_expr(tokens, pos)
        }
    }
}

/// Evaluate a parsed tsquery tree against a tsvector.
fn eval_tsquery_node(
    node: &TsQueryNode,
    words: &[String],
    positions: &std::collections::HashMap<String, Vec<usize>>,
) -> bool {
    match node {
        TsQueryNode::Term(term) => words.iter().any(|w| w == term),
        TsQueryNode::And(left, right) => {
            eval_tsquery_node(left, words, positions) && eval_tsquery_node(right, words, positions)
        }
        TsQueryNode::Or(left, right) => {
            eval_tsquery_node(left, words, positions) || eval_tsquery_node(right, words, positions)
        }
        TsQueryNode::Not(inner) => !eval_tsquery_node(inner, words, positions),
        TsQueryNode::Phrase(terms) => {
            // Check if terms appear consecutively in the tsvector
            if terms.is_empty() {
                return false;
            }
            // Get positions for the first term
            let first_positions = match positions.get(&terms[0]) {
                Some(p) => p.clone(),
                None => return false,
            };
            // For each starting position, check if subsequent terms follow
            'outer: for &start_pos in &first_positions {
                for (offset, term) in terms.iter().enumerate().skip(1) {
                    let expected_pos = start_pos + offset;
                    match positions.get(term) {
                        Some(ps) if ps.contains(&expected_pos) => {}
                        _ => continue 'outer,
                    }
                }
                return true;
            }
            false
        }
    }
}

/// Evaluate a tsquery string against a tsvector's words and positions.
fn evaluate_tsquery(
    tsquery_str: &str,
    words: &[String],
    positions: &std::collections::HashMap<String, Vec<usize>>,
) -> bool {
    match parse_tsquery_expr(tsquery_str) {
        Some(node) => eval_tsquery_node(&node, words, positions),
        None => false,
    }
}

/// Highlight matching words in text with `<b>...</b>` tags.
fn highlight_text(text: &str, query_words: &[String]) -> String {
    let mut result = String::new();
    let mut i = 0;
    let bytes = text.as_bytes();
    let len = bytes.len();

    while i < len {
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &text[start..i];
            let lower = word.to_lowercase();
            if query_words
                .iter()
                .any(|qw| lower == *qw || lower.starts_with(qw.as_str()))
            {
                result.push_str("<b>");
                result.push_str(word);
                result.push_str("</b>");
            } else {
                result.push_str(word);
            }
        } else {
            result.push(text.as_bytes()[i] as char);
            i += 1;
        }
    }
    result
}

/// Parse web search syntax into a tsquery string.
fn parse_websearch(input: &str, use_english: bool) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut parts: Vec<String> = Vec::new();
    let mut chars = trimmed.chars().peekable();
    let mut pending_or = false;

    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }

        if chars.peek().is_none() {
            break;
        }

        let Some(&c) = chars.peek() else {
            break;
        };

        if c == '"' {
            chars.next();
            let mut phrase = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == '"' {
                    chars.next();
                    break;
                }
                phrase.push(nc);
                chars.next();
            }
            let words = tokenize_and_filter(&phrase, use_english);
            if !words.is_empty() {
                let phrase_query = build_tsquery_join(&words, " <-> ");
                if pending_or {
                    if let Some(last) = parts.last_mut() {
                        *last = format!("{last} | {phrase_query}");
                    } else {
                        parts.push(phrase_query);
                    }
                    pending_or = false;
                } else {
                    parts.push(phrase_query);
                }
            }
        } else if c == '-' {
            chars.next();
            let mut word = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_alphanumeric() || nc == '_' {
                    word.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            if !word.is_empty() {
                let lower = word.to_lowercase();
                if !use_english || !is_english_stop_word(&lower) {
                    let stemmed = if use_english {
                        english_stem(&lower)
                    } else {
                        lower
                    };
                    parts.push(format!("!'{stemmed}'"));
                }
            }
        } else if c.is_alphanumeric() || c == '_' {
            let mut word = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_alphanumeric() || nc == '_' {
                    word.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            let lower = word.to_lowercase();
            if lower == "or" {
                pending_or = true;
            } else {
                if use_english && is_english_stop_word(&lower) {
                    continue;
                }
                let stemmed = if use_english {
                    english_stem(&lower)
                } else {
                    lower
                };
                if pending_or {
                    if let Some(last) = parts.last_mut() {
                        *last = format!("{last} | '{stemmed}'");
                    } else {
                        parts.push(format!("'{stemmed}'"));
                    }
                    pending_or = false;
                } else {
                    parts.push(format!("'{stemmed}'"));
                }
            }
        } else {
            chars.next();
        }
    }

    parts.join(" & ")
}

#[cfg(test)]
mod tests {
    use super::normalize_tsquery;

    #[test]
    fn normalize_tsquery_handles_weight_suffixes_without_hanging() {
        let normalized = normalize_tsquery("footballyklubber:b & rebookings:A & sky", true);
        assert!(!normalized.is_empty());
        assert!(normalized.contains("':b"));
        assert!(normalized.contains("':A"));
    }

    #[test]
    fn normalize_tsquery_makes_progress_on_unrecognized_punctuation() {
        let normalized = normalize_tsquery("::: ???", false);
        assert_eq!(normalized, "");
    }
}
