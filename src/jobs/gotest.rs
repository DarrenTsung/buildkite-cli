use crate::jobs::{JobParser, JobResult};
use crate::log_parser::CleanLine;
use regex::Regex;
use serde::Serialize;

pub struct GoTestParser;

#[derive(Debug, Serialize)]
pub struct GoTestResult {
    pub packages: Vec<GoTestPackage>,
    pub bazel_summary: Option<BazelSummary>,
}

#[derive(Debug, Serialize)]
pub struct GoTestPackage {
    pub target: String,
    pub passed: bool,
    pub tests: Vec<GoTest>,
    pub failing_tests: Vec<GoFailingTest>,
}

#[derive(Debug, Serialize)]
pub struct GoTest {
    pub name: String,
    pub status: GoTestStatus,
    pub duration_secs: f64,
}

#[derive(Debug, Serialize)]
pub struct GoFailingTest {
    pub name: String,
    pub duration_secs: f64,
    pub output: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum GoTestStatus {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Serialize)]
pub struct BazelSummary {
    pub executed: usize,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
}

impl JobParser for GoTestParser {
    fn parse(&self, lines: &[CleanLine]) -> JobResult {
        let result = parse_go_tests(lines);
        JobResult::GoTest(result)
    }
}

fn parse_go_tests(lines: &[CleanLine]) -> GoTestResult {
    let target_re = Regex::new(r"^={20} Test output for (//[^:]+:\S+):$").unwrap();
    let run_re = Regex::new(r"^=== RUN\s+(\S+)").unwrap();
    let result_re = Regex::new(r"^--- (PASS|FAIL|SKIP): (\S+) \(([\d.]+)s\)$").unwrap();
    let bazel_summary_re =
        Regex::new(r"^Executed (\d+) out of (\d+) tests?: (.+)\.$").unwrap();

    let mut packages: Vec<GoTestPackage> = Vec::new();
    let mut current_target: Option<String> = None;
    let mut current_tests: Vec<GoTest> = Vec::new();
    let mut current_failures: Vec<GoFailingTest> = Vec::new();
    let mut bazel_summary = None;

    // Track output lines for the currently running test.
    // In Go, failure output appears between === RUN and --- FAIL.
    let mut current_run_name: Option<String> = None;
    let mut current_run_output: Vec<String> = Vec::new();

    for line in lines {
        let text = line.text.trim();

        // New bazel test target
        if let Some(caps) = target_re.captures(text) {
            flush_package(
                &mut packages,
                &mut current_target,
                &mut current_tests,
                &mut current_failures,
            );
            current_target = Some(caps[1].to_string());
            current_run_name = None;
            current_run_output.clear();
            continue;
        }

        // === RUN TestFoo — start of a new test
        if let Some(caps) = run_re.captures(text) {
            // New test starting — reset output buffer
            current_run_name = Some(caps[1].to_string());
            current_run_output.clear();
            continue;
        }

        // --- PASS/FAIL/SKIP: TestFoo (0.00s)
        if let Some(caps) = result_re.captures(text) {
            let status_str = &caps[1];
            let name = caps[2].to_string();
            let duration: f64 = caps[3].parse().unwrap_or(0.0);

            let status = match status_str {
                "PASS" => GoTestStatus::Pass,
                "FAIL" => GoTestStatus::Fail,
                "SKIP" => GoTestStatus::Skip,
                _ => continue,
            };

            current_tests.push(GoTest {
                name: name.clone(),
                status: status.clone(),
                duration_secs: duration,
            });

            if status == GoTestStatus::Fail {
                // Use the output collected since the matching === RUN
                let output = if current_run_name.as_deref() == Some(&name) {
                    std::mem::take(&mut current_run_output)
                } else {
                    Vec::new()
                };
                current_failures.push(GoFailingTest {
                    name,
                    duration_secs: duration,
                    output,
                });
            }

            current_run_name = None;
            current_run_output.clear();
            continue;
        }

        // Bazel summary: "Executed 3 out of 92 tests: 92 tests pass."
        if let Some(caps) = bazel_summary_re.captures(text) {
            flush_package(
                &mut packages,
                &mut current_target,
                &mut current_tests,
                &mut current_failures,
            );

            let executed: usize = caps[1].parse().unwrap_or(0);
            let total: usize = caps[2].parse().unwrap_or(0);
            let rest = &caps[3];

            let passed = extract_count(rest, "pass");
            let failed = extract_count(rest, "fail");

            bazel_summary = Some(BazelSummary {
                executed,
                total,
                passed,
                failed,
            });
            continue;
        }

        // Collect output lines while a test is running (for failure capture)
        if current_run_name.is_some() && !text.is_empty() && !text.starts_with("=== ") {
            current_run_output.push(text.to_string());
        }
    }

    // Flush last package
    flush_package(
        &mut packages,
        &mut current_target,
        &mut current_tests,
        &mut current_failures,
    );

    GoTestResult {
        packages,
        bazel_summary,
    }
}

fn flush_package(
    packages: &mut Vec<GoTestPackage>,
    target: &mut Option<String>,
    tests: &mut Vec<GoTest>,
    failures: &mut Vec<GoFailingTest>,
) {
    if let Some(target) = target.take() {
        let passed = failures.is_empty();
        packages.push(GoTestPackage {
            target,
            passed,
            tests: std::mem::take(tests),
            failing_tests: std::mem::take(failures),
        });
    }
}

fn extract_count(s: &str, label: &str) -> usize {
    // Matches "92 tests pass" or "1 fail" etc.
    let pattern = format!(r"(\d+)\s+\w*\s*{}", regex::escape(label));
    Regex::new(&pattern)
        .ok()
        .and_then(|re| re.captures(s))
        .and_then(|caps| caps[1].parse().ok())
        .unwrap_or(0)
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
    fn test_parse_passing_package() {
        let input = lines(&[
            "==================== Test output for //pkg:pkg_test:",
            "=== RUN   TestFoo",
            "--- PASS: TestFoo (0.01s)",
            "=== RUN   TestBar",
            "--- PASS: TestBar (0.02s)",
            "PASS",
        ]);
        let result = parse_go_tests(&input);
        assert_eq!(result.packages.len(), 1);
        let pkg = &result.packages[0];
        assert_eq!(pkg.target, "//pkg:pkg_test");
        assert!(pkg.passed);
        assert_eq!(pkg.tests.len(), 2);
        assert_eq!(pkg.failing_tests.len(), 0);
    }

    #[test]
    fn test_parse_failing_test() {
        let input = lines(&[
            "==================== Test output for //pkg:pkg_test:",
            "=== RUN   TestFoo",
            "    foo_test.go:42: expected 1, got 2",
            "    foo_test.go:43: another error",
            "--- FAIL: TestFoo (0.05s)",
            "FAIL",
        ]);
        let result = parse_go_tests(&input);
        assert_eq!(result.packages.len(), 1);
        let pkg = &result.packages[0];
        assert!(!pkg.passed);
        assert_eq!(pkg.failing_tests.len(), 1);
        let ft = &pkg.failing_tests[0];
        assert_eq!(ft.name, "TestFoo");
        assert_eq!(ft.output.len(), 2);
        assert_eq!(ft.output[0], "foo_test.go:42: expected 1, got 2");
    }

    #[test]
    fn test_parse_bazel_summary() {
        let input = lines(&[
            "Executed 3 out of 92 tests: 92 tests pass.",
        ]);
        let result = parse_go_tests(&input);
        let summary = result.bazel_summary.unwrap();
        assert_eq!(summary.executed, 3);
        assert_eq!(summary.total, 92);
        assert_eq!(summary.passed, 92);
        assert_eq!(summary.failed, 0);
    }

    #[test]
    fn test_parse_subtests() {
        let input = lines(&[
            "==================== Test output for //pkg:pkg_test:",
            "=== RUN   TestValidate",
            "=== RUN   TestValidate/case_one",
            "--- PASS: TestValidate/case_one (0.00s)",
            "=== RUN   TestValidate/case_two",
            "    validate_test.go:10: wrong",
            "--- FAIL: TestValidate/case_two (0.00s)",
            "--- FAIL: TestValidate (0.01s)",
            "FAIL",
        ]);
        let result = parse_go_tests(&input);
        let pkg = &result.packages[0];
        assert!(!pkg.passed);
        // Should capture the subtest failure and the parent
        let fail_names: Vec<&str> = pkg.failing_tests.iter().map(|f| f.name.as_str()).collect();
        assert!(fail_names.contains(&"TestValidate/case_two"));
    }
}
