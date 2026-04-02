pub mod golint;
pub mod gotest;
pub mod mocha;
pub mod nextest;
pub mod script_error;

use crate::log_parser::CleanLine;
use regex::Regex;
use serde::Serialize;
use std::collections::HashSet;

pub trait JobParser {
    fn parse(&self, lines: &[CleanLine]) -> JobResult;
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum JobResult {
    #[serde(rename = "nextest")]
    Nextest(nextest::NextestResult),
    #[serde(rename = "mocha")]
    Mocha(mocha::MochaResult),
    #[serde(rename = "gotest")]
    GoTest(gotest::GoTestResult),
    #[serde(rename = "golint")]
    GoLint(golint::GoLintResult),
    #[serde(rename = "script_error")]
    ScriptError(script_error::ScriptErrorResult),
}

impl JobResult {
    /// Returns true when a specialized parser found no domain-specific results.
    /// Used to trigger fallback to the generic ScriptErrorParser.
    pub fn is_empty(&self) -> bool {
        match self {
            JobResult::Nextest(r) => r.runs.is_empty(),
            JobResult::Mocha(r) => r.passing == 0 && r.failing == 0,
            JobResult::GoTest(r) => r.packages.is_empty() && r.bazel_summary.is_none(),
            JobResult::GoLint(r) => r.issues.is_empty(),
            JobResult::ScriptError(_) => false, // Already the fallback
        }
    }

    /// Returns all output text that the parser captured (failure output,
    /// error messages, raw output, etc.). Used by the uncaptured error
    /// scanner to determine what the parser already covered.
    pub fn collected_output(&self) -> HashSet<String> {
        let mut collected = HashSet::new();
        match self {
            JobResult::Nextest(r) => {
                for run in &r.runs {
                    for ft in &run.failing_tests {
                        for line in &ft.stdout {
                            collected.insert(line.trim().to_string());
                        }
                        for line in &ft.stderr {
                            collected.insert(line.trim().to_string());
                        }
                    }
                }
            }
            JobResult::Mocha(r) => {
                for f in &r.failures {
                    if !f.error_message.is_empty() {
                        collected.insert(f.error_message.trim().to_string());
                    }
                    for line in &f.stack_trace {
                        collected.insert(line.trim().to_string());
                    }
                }
            }
            JobResult::GoTest(r) => {
                for pkg in &r.packages {
                    for ft in &pkg.failing_tests {
                        for line in &ft.output {
                            collected.insert(line.trim().to_string());
                        }
                    }
                    for line in &pkg.raw_output {
                        collected.insert(line.trim().to_string());
                    }
                }
            }
            JobResult::GoLint(r) => {
                for issue in &r.issues {
                    collected.insert(issue.message.trim().to_string());
                }
            }
            JobResult::ScriptError(r) => {
                for err in &r.errors {
                    collected.insert(err.trim().to_string());
                }
            }
        }
        collected
    }
}

/// Heuristic: does this line look like an error in CI output?
/// Shared across all parsers for consistent error detection.
pub fn line_looks_like_error(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    // Bare "FAIL" / "PASS" / "ok" are Go test framework status markers, not errors.
    // "--- FAIL:" is a Go test result line captured by the Go parser.
    if line == "FAIL" || line == "PASS" || line.starts_with("ok ") || line.starts_with("--- ") {
        return false;
    }
    // Common error indicators across languages
    line.starts_with("Error")
        || line.starts_with("error")
        || line.starts_with("FAIL ")
        || line.starts_with("FATAL")
        || line.starts_with("panic:")
        || line.contains("Error:")
        || line.contains("FAILED")
        || line.contains("exit code 1")
        || line.contains("Exit 1")
        || line.contains("AssertionError")
        || line.contains("TypeError")
        || line.contains("ReferenceError")
        || line.contains("timed out")
        || line.contains("TIMED OUT")
}

/// CI infrastructure noise that should not be flagged as uncaptured errors.
fn is_ci_noise(line: &str) -> bool {
    let line = line.trim();
    // Buildkite/CI infrastructure messages
    line.contains("artifact")
        || line.contains("upload")
        || line.contains("docker")
        || line.contains("datadog")
        || line.contains("buildkite")
        || line.contains("ci-interp")
        || line.contains("hooks/")
        || line.contains("ssh")
        || line.contains("revoking")
        || line.contains("pruning")
        || line.contains("ci-cache")
        || line.contains("retry-cli")
        || line.contains("test-processor")
        || line.contains("git trace")
        // Bazel progress/infra noise
        || line.starts_with("INFO:")
        || line.starts_with("WARNING:")
        || line.contains("Build did NOT complete")
        || line.contains("build interrupted")
        || line.contains("ERROR: Build")
}

/// Scan all log lines for error patterns and return those not already
/// captured by the parser's result. This catches the class of bug where
/// a parser "succeeds" on some output but silently ignores failures from
/// other targets or languages in the same log.
pub fn find_uncaptured_errors(lines: &[CleanLine], result: &JobResult) -> Vec<String> {
    // ScriptError is already the catch-all; don't double-check it.
    if matches!(result, JobResult::ScriptError(_)) {
        return Vec::new();
    }

    let collected = result.collected_output();

    // Also collect Bazel-level error lines that are structural, not test output
    let bazel_failed_re = Regex::new(r"^//\S+\s+(FAILED|TIMEOUT)").unwrap();

    let mut uncaptured = Vec::new();
    for line in lines {
        let text = line.text.trim();
        if !line_looks_like_error(text) && !bazel_failed_re.is_match(text) {
            continue;
        }
        if is_ci_noise(text) {
            continue;
        }
        // Check if this error is already represented in the parser output
        if collected.contains(text) {
            continue;
        }
        // Check substring containment (parser may have captured a superset)
        if collected.iter().any(|c| c.contains(text) || text.contains(c.as_str())) {
            continue;
        }
        uncaptured.push(text.to_string());
    }
    uncaptured
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_parser::CleanLine;

    fn lines(texts: &[&str]) -> Vec<CleanLine> {
        texts
            .iter()
            .map(|t| CleanLine {
                text: t.to_string(),
                timestamp_ms: None,
            })
            .collect()
    }

    #[test]
    fn test_uncaptured_errors_detected() {
        // GoTest parser finds passing Go tests but misses a TS error
        let input = lines(&[
            "==================== Test output for //pkg:pkg_test:",
            "=== RUN   TestFoo",
            "--- PASS: TestFoo (0.01s)",
            "Error [AgentPlatError]: bootstrap config is required",
            "Executed 1 out of 1 tests: 1 tests pass.",
        ]);
        let executed = crate::log_parser::extract_executed_targets("");
        let parser = gotest::GoTestParser { executed_targets: executed };
        let result = parser.parse(&input);
        let uncaptured = find_uncaptured_errors(&input, &result);
        assert!(
            uncaptured.iter().any(|e| e.contains("bootstrap config")),
            "should detect uncaptured TS error, got: {:?}",
            uncaptured
        );
    }

    #[test]
    fn test_no_false_positives_when_errors_captured() {
        // GoTest parser captures the failure, no uncaptured errors
        let input = lines(&[
            "==================== Test output for //pkg:pkg_test:",
            "=== RUN   TestFoo",
            "    Error: something went wrong",
            "--- FAIL: TestFoo (0.05s)",
            "FAIL",
        ]);
        let executed = crate::log_parser::extract_executed_targets("");
        let parser = gotest::GoTestParser { executed_targets: executed };
        let result = parser.parse(&input);
        let uncaptured = find_uncaptured_errors(&input, &result);
        assert!(
            uncaptured.is_empty(),
            "captured errors should not be flagged, got: {:?}",
            uncaptured
        );
    }

    #[test]
    fn test_script_error_skips_uncaptured_scan() {
        // ScriptError is the catch-all, don't double-check it
        let input = lines(&[
            "Error: something bad happened",
            "exit status 1",
        ]);
        let parser = script_error::ScriptErrorParser;
        let result = parser.parse(&input);
        let uncaptured = find_uncaptured_errors(&input, &result);
        assert!(uncaptured.is_empty());
    }

    #[test]
    fn test_uncaptured_bazel_failed_line() {
        // A FAILED target that the parser didn't capture
        let input = lines(&[
            "==================== Test output for //go:go_test:",
            "=== RUN   TestOk",
            "--- PASS: TestOk (0.01s)",
            "//ts/itest:itest   FAILED in 5.0s",
            "Executed 2 out of 2 tests: 1 tests pass and 1 fail locally.",
        ]);
        let executed = crate::log_parser::extract_executed_targets("");
        let parser = gotest::GoTestParser { executed_targets: executed };
        let result = parser.parse(&input);
        // The gotest parser should capture this via add_missing_failed_targets,
        // but the uncaptured scanner also catches FAILED lines as a safety net
        let uncaptured = find_uncaptured_errors(&input, &result);
        // Either the parser caught it (no uncaptured) or the scanner caught it
        // Both are acceptable outcomes
        let parser_caught = match &result {
            JobResult::GoTest(r) => r.packages.iter().any(|p| !p.passed && p.target.contains("ts/itest")),
            _ => false,
        };
        assert!(
            parser_caught || !uncaptured.is_empty(),
            "FAILED target should be caught by parser or scanner"
        );
    }
}

pub fn classify(job_name: &str, raw_log: &str) -> Box<dyn JobParser> {
    if job_name.contains("rust-tests") || job_name.contains("nextest") {
        Box::new(nextest::NextestParser)
    } else if job_name.contains("typescript-tests") || job_name.contains("mocha") {
        Box::new(mocha::MochaParser)
    } else if job_name.contains("lint") {
        Box::new(golint::GoLintParser)
    } else if job_name.contains("agentplat") || job_name.contains("go-test") {
        let executed_targets = crate::log_parser::extract_executed_targets(raw_log);
        Box::new(gotest::GoTestParser { executed_targets })
    } else {
        // Fall back to generic script error parser for any unrecognized job
        Box::new(script_error::ScriptErrorParser)
    }
}
