//! Minimal Gherkin parser for openCypher TCK `.feature` files.
//!
//! Parses the subset of Gherkin used by the TCK: Feature, Scenario,
//! Scenario Outline, Given/When/Then/And steps, doc-strings, and data tables.

use std::path::Path;

// ── AST ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Feature {
    pub _name: String,
    pub scenarios: Vec<Scenario>,
}

#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String,
    pub tags: Vec<String>,
    pub steps: Vec<Step>,
    /// For Scenario Outline: each row in the Examples table is a map of
    /// placeholder → value.  Regular Scenarios have an empty vec.
    pub examples: Vec<Vec<(String, String)>>,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub keyword: StepKeyword,
    pub text: String,
    pub doc_string: Option<String>,
    pub table: Option<Vec<Vec<String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKeyword {
    Given,
    When,
    Then,
    And,
    But,
}

// ── Parser ───────────────────────────────────────────────────────────

pub fn parse_feature_file(path: &Path) -> Result<Feature, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_feature(&content)
}

pub fn parse_feature(input: &str) -> Result<Feature, String> {
    let lines: Vec<&str> = input.lines().collect();
    let mut idx = 0;
    let mut feature_name = String::new();
    let mut scenarios = Vec::new();

    // Skip to Feature: line
    while idx < lines.len() {
        let trimmed = lines[idx].trim();
        if let Some(rest) = trimmed.strip_prefix("Feature:") {
            feature_name = rest.trim().to_string();
            idx += 1;
            break;
        }
        idx += 1;
    }

    // Parse scenarios
    while idx < lines.len() {
        let trimmed = lines[idx].trim();

        // Collect tags
        if trimmed.starts_with('@') {
            let tags: Vec<String> = trimmed
                .split_whitespace()
                .filter(|t| t.starts_with('@'))
                .map(|t| t.to_string())
                .collect();
            idx += 1;
            // Next line should be Scenario
            if idx < lines.len() {
                let next = lines[idx].trim();
                if let Some(name) = strip_scenario_prefix(next) {
                    idx += 1;
                    let (steps, examples, new_idx) = parse_scenario_body(&lines, idx);
                    idx = new_idx;
                    scenarios.push(Scenario {
                        name,
                        tags,
                        steps,
                        examples,
                    });
                    continue;
                }
            }
            continue;
        }

        if let Some(name) = strip_scenario_prefix(trimmed) {
            idx += 1;
            let (steps, examples, new_idx) = parse_scenario_body(&lines, idx);
            idx = new_idx;
            scenarios.push(Scenario {
                name,
                tags: Vec::new(),
                steps,
                examples,
            });
            continue;
        }

        idx += 1;
    }

    Ok(Feature {
        _name: feature_name,
        scenarios,
    })
}

fn strip_scenario_prefix(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("Scenario Outline:") {
        Some(rest.trim().to_string())
    } else if let Some(rest) = line.strip_prefix("Scenario:") {
        Some(rest.trim().to_string())
    } else {
        None
    }
}

fn parse_scenario_body(
    lines: &[&str],
    mut idx: usize,
) -> (Vec<Step>, Vec<Vec<(String, String)>>, usize) {
    let mut steps = Vec::new();
    let mut examples = Vec::new();

    while idx < lines.len() {
        let trimmed = lines[idx].trim();

        // Stop at next scenario, feature, or tag line (unless inside Examples)
        if trimmed.starts_with("Scenario:") || trimmed.starts_with("Scenario Outline:") {
            break;
        }
        if trimmed.starts_with('@') && !trimmed.contains("skip") {
            // Could be tags for next scenario — peek ahead
            if idx + 1 < lines.len() {
                let next = lines[idx + 1].trim();
                if next.starts_with("Scenario:") || next.starts_with("Scenario Outline:") {
                    break;
                }
            }
        }

        // Examples section (for Scenario Outline)
        if trimmed.starts_with("Examples:") {
            idx += 1;
            let (table, new_idx) = parse_data_table(lines, idx);
            idx = new_idx;
            if let Some(table) = table {
                if table.len() > 1 {
                    let headers = &table[0];
                    for row in &table[1..] {
                        let pairs: Vec<(String, String)> = headers
                            .iter()
                            .zip(row.iter())
                            .map(|(h, v)| (h.clone(), v.clone()))
                            .collect();
                        examples.push(pairs);
                    }
                }
            }
            continue;
        }

        // Parse step
        if let Some(keyword) = parse_step_keyword(trimmed) {
            let text = trim_step_keyword(trimmed).to_string();
            idx += 1;

            // Check for doc-string
            let mut doc_string = None;
            if idx < lines.len() && lines[idx].trim().starts_with("\"\"\"") {
                idx += 1; // skip opening """
                let mut doc = Vec::new();
                while idx < lines.len() && !lines[idx].trim().starts_with("\"\"\"") {
                    doc.push(lines[idx].trim_start());
                    idx += 1;
                }
                if idx < lines.len() {
                    idx += 1; // skip closing """
                }
                doc_string = Some(doc.join("\n"));
            }

            // Check for data table
            let mut table = None;
            if idx < lines.len() && lines[idx].trim().starts_with('|') {
                let (t, new_idx) = parse_data_table(lines, idx);
                idx = new_idx;
                table = t;
            }

            steps.push(Step {
                keyword,
                text,
                doc_string,
                table,
            });
            continue;
        }

        idx += 1;
    }

    (steps, examples, idx)
}

fn parse_step_keyword(line: &str) -> Option<StepKeyword> {
    if line.starts_with("Given ") {
        Some(StepKeyword::Given)
    } else if line.starts_with("When ") {
        Some(StepKeyword::When)
    } else if line.starts_with("Then ") {
        Some(StepKeyword::Then)
    } else if line.starts_with("And ") {
        Some(StepKeyword::And)
    } else if line.starts_with("But ") {
        Some(StepKeyword::But)
    } else {
        None
    }
}

fn trim_step_keyword(line: &str) -> &str {
    for prefix in ["Given ", "When ", "Then ", "And ", "But "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return rest;
        }
    }
    line
}

fn parse_data_table(lines: &[&str], mut idx: usize) -> (Option<Vec<Vec<String>>>, usize) {
    let mut rows = Vec::new();
    while idx < lines.len() {
        let trimmed = lines[idx].trim();
        if !trimmed.starts_with('|') {
            break;
        }
        let cells: Vec<String> = trimmed
            .split('|')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().to_string())
            .collect();
        if !cells.is_empty() {
            rows.push(cells);
        }
        idx += 1;
    }
    if rows.is_empty() {
        (None, idx)
    } else {
        (Some(rows), idx)
    }
}
