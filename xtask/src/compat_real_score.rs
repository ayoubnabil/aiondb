//! `cargo xtask compat-real-score` - report raw score vs no-op-excluded score.

use std::fs;
use std::path::PathBuf;

const MATRIX_PATH: &str = "crates/aiondb-pg-compat/src/compat_tag_matrix.rs";

#[derive(Clone, Debug)]
pub struct CompatRealScoreOptions {
    pub json: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct MatrixScore {
    implemented_real: usize,
    explicit_not_supported: usize,
    intentional_noop: Vec<String>,
}

impl MatrixScore {
    fn total(&self) -> usize {
        self.implemented_real + self.explicit_not_supported + self.intentional_noop.len()
    }

    fn raw_matched(&self) -> usize {
        self.implemented_real + self.intentional_noop.len()
    }

    fn real_matched(&self) -> usize {
        self.implemented_real
    }
}

pub fn parse_args(args: &[String]) -> Result<CompatRealScoreOptions, String> {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask compat-real-score [--json]\n\n\
Reports raw compatibility-matrix acceptance and the real score after excluding \
IntentionalNoop tags from the numerator."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(CompatRealScoreOptions { json })
}

pub fn run(opts: CompatRealScoreOptions) -> Result<(), String> {
    let root = repo_root();
    let matrix_path = root.join(MATRIX_PATH);
    let src = fs::read_to_string(&matrix_path)
        .map_err(|error| format!("reading {}: {error}", matrix_path.display()))?;
    let score = parse_matrix_score(&src)?;
    if opts.json {
        print_json(&score);
    } else {
        print_human(&score);
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

fn parse_matrix_score(src: &str) -> Result<MatrixScore, String> {
    let mut score = MatrixScore::default();
    let mut current_tag: Option<String> = None;

    for line in src.lines() {
        let trimmed = line.trim();
        if let Some(tag) = parse_tag_line(trimmed) {
            current_tag = Some(tag);
            continue;
        }
        if trimmed.contains("behavior: CompatTagBehavior::ImplementedReal") {
            score.implemented_real += 1;
            current_tag = None;
        } else if trimmed.contains("behavior: CompatTagBehavior::ExplicitNotSupported") {
            score.explicit_not_supported += 1;
            current_tag = None;
        } else if trimmed.contains("behavior: CompatTagBehavior::IntentionalNoop") {
            let Some(tag) = current_tag.take() else {
                return Err("IntentionalNoop entry missing preceding tag".to_owned());
            };
            score.intentional_noop.push(tag);
        }
    }

    score.intentional_noop.sort();
    Ok(score)
}

fn parse_tag_line(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("tag: \"")?;
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

fn pct(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64 * 100.0
    }
}

fn print_human(score: &MatrixScore) {
    let total = score.total();
    println!("== compat-real-score ==");
    println!(
        "raw score:  {}/{} ({:.1}%)",
        score.raw_matched(),
        total,
        pct(score.raw_matched(), total)
    );
    println!(
        "real score: {}/{} ({:.1}%)",
        score.real_matched(),
        total,
        pct(score.real_matched(), total)
    );
    println!("intentional no-op tags: {}", score.intentional_noop.len());
    for tag in &score.intentional_noop {
        println!("  - {tag}");
    }
}

fn print_json(score: &MatrixScore) {
    let total = score.total();
    println!(
        "{{\"total\":{},\"raw_matched\":{},\"raw_pct\":{:.2},\"real_matched\":{},\"real_pct\":{:.2},\"intentional_noop_tags\":[{}]}}",
        total,
        score.raw_matched(),
        pct(score.raw_matched(), total),
        score.real_matched(),
        pct(score.real_matched(), total),
        score
            .intentional_noop
            .iter()
            .map(|tag| format!("\"{}\"", json_escape(tag)))
            .collect::<Vec<_>>()
            .join(",")
    );
}

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_intentional_noops_from_real_score() {
        let src = r#"
CompatTagEntry {
    tag: "ALTER TABLE",
    behavior: CompatTagBehavior::ImplementedReal,
},
CompatTagEntry {
    tag: "GRANT",
    behavior: CompatTagBehavior::ImplementedReal,
},
CompatTagEntry {
    tag: "REVOKE",
    behavior: CompatTagBehavior::ImplementedReal,
},
"#;
        let score = parse_matrix_score(src).expect("score");
        assert_eq!(score.total(), 3);
        assert_eq!(score.raw_matched(), 3);
        assert_eq!(score.real_matched(), 3);
        assert!(score.intentional_noop.is_empty());
    }
}
