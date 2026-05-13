use std::time::Duration;

#[derive(Debug)]
pub struct SuiteStats {
    pub passed: usize,
    pub skipped: usize,
}

pub type SuiteResult = Result<SuiteStats, Vec<String>>;

pub struct Report {
    total_passed: usize,
    total_skipped: usize,
    total_failed: usize,
    suite_count: usize,
    failed_suites: Vec<String>,
}

impl Report {
    pub fn new() -> Self {
        Self {
            total_passed: 0,
            total_skipped: 0,
            total_failed: 0,
            suite_count: 0,
            failed_suites: Vec::new(),
        }
    }

    pub fn record(&mut self, name: &str, result: SuiteResult) {
        self.suite_count += 1;
        match result {
            Ok(stats) => {
                self.total_passed += stats.passed;
                self.total_skipped += stats.skipped;
            }
            Err(failures) => {
                self.total_failed += failures.len();
                self.failed_suites.push(name.to_owned());
            }
        }
    }

    pub fn all_passed(&self) -> bool {
        self.total_failed == 0
    }

    pub fn summary(&self, elapsed: Duration) {
        eprintln!("=== INTEGRITY REPORT ===");
        eprintln!(
            "Suites: {} total, {} failed",
            self.suite_count,
            self.failed_suites.len()
        );
        eprintln!(
            "Tests:  {} passed, {} skipped, {} failed",
            self.total_passed, self.total_skipped, self.total_failed
        );
        eprintln!("Time:   {:.2}s", elapsed.as_secs_f64());
        if !self.failed_suites.is_empty() {
            eprintln!();
            eprintln!("FAILED SUITES:");
            for s in &self.failed_suites {
                eprintln!("  - {s}");
            }
        }
    }
}
