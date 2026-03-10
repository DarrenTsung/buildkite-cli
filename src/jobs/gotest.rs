use crate::jobs::{JobParser, JobResult};
use crate::log_parser::CleanLine;
use regex::Regex;
use serde::Serialize;

pub struct GoTestParser {
    /// Truncated Bazel target patterns for targets that were actually executed (duration > 0s)
    pub executed_targets: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GoTestResult {
    pub packages: Vec<GoTestPackage>,
    pub bazel_summary: Option<BazelSummary>,
}

#[derive(Debug, Serialize)]
pub struct GoTestPackage {
    pub target: String,
    pub passed: bool,
    /// Whether this target was actually executed (not a Bazel cache hit)
    pub executed: bool,
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
        let mut result = parse_go_tests(lines);
        // Mark packages as executed based on Bazel progress line durations.
        // Truncated targets like `//.../bootstrap:bootstrap_test` are matched
        // against full targets by checking if the full target ends with the
        // suffix after `...`.
        for pkg in &mut result.packages {
            pkg.executed = self.executed_targets.iter().any(|et| target_matches(&pkg.target, et));
        }
        JobResult::GoTest(result)
    }
}

/// Check if a full target like `//a/b/c:c_test` matches a potentially truncated
/// target like `//.../c:c_test`.
fn target_matches(full: &str, pattern: &str) -> bool {
    if full == pattern {
        return true;
    }
    // Pattern like `//.../foo:bar_test` — match suffix after `...`
    if let Some(suffix) = pattern.strip_prefix("//...") {
        full.ends_with(suffix)
    } else {
        false
    }
}

fn parse_go_tests(lines: &[CleanLine]) -> GoTestResult {
    let target_re = Regex::new(r"^={20} Test output for (//[^:]+:\S+):$").unwrap();
    // Bazel streaming prefix: @@//target:binary | <content>
    let streaming_re = Regex::new(r"^@@(//\S+)\s+\|\s+(.*)$").unwrap();
    let run_re = Regex::new(r"^=== RUN\s+(\S+)").unwrap();
    let result_re = Regex::new(r"^--- (PASS|FAIL|SKIP): (\S+) \(([\d.]+)s\)$").unwrap();
    let bazel_summary_re =
        Regex::new(r"^Executed (\d+) out of (\d+) tests?: (.+)\.$").unwrap();

    let mut packages: Vec<GoTestPackage> = Vec::new();
    let mut current_target: Option<String> = None;
    let mut current_tests: Vec<GoTest> = Vec::new();
    let mut current_failures: Vec<GoFailingTest> = Vec::new();
    let mut bazel_summary = None;

    // For streaming format, collect lines per target first since they can
    // be interleaved. Key is the target (package path portion).
    let mut streaming_lines: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut has_streaming = false;

    // Track output lines for the currently running test.
    // In Go, failure output appears between === RUN and --- FAIL.
    let mut current_run_name: Option<String> = None;
    let mut current_run_output: Vec<String> = Vec::new();

    // First pass: separate streaming lines by target, collect non-streaming lines
    let mut non_streaming_lines: Vec<CleanLine> = Vec::new();
    for line in lines {
        let text = line.text.trim();
        if let Some(caps) = streaming_re.captures(text) {
            has_streaming = true;
            let target = caps[1].to_string();
            let content = caps[2].to_string();
            // Group by the package path (strip the binary name after colon)
            // e.g. //a/b/c:scenario_test and //a/b/c:sboxd both go under //a/b/c
            let pkg_key = if let Some(colon) = target.rfind(':') {
                target[..colon].to_string()
            } else {
                target.clone()
            };
            streaming_lines
                .entry(pkg_key)
                .or_default()
                .push(content);
        } else {
            non_streaming_lines.push(line.clone());
        }
    }

    // If we have streaming output, parse each target group
    if has_streaming {
        for (target, content_lines) in &streaming_lines {
            let fake_lines: Vec<CleanLine> = content_lines
                .iter()
                .map(|t| CleanLine {
                    text: t.clone(),
                    timestamp_ms: None,
                })
                .collect();
            let group = parse_go_test_group(&fake_lines);
            if !group.tests.is_empty() || !group.failures.is_empty() {
                packages.push(GoTestPackage {
                    target: target.clone(),
                    passed: group.failures.is_empty(),
                    executed: true, // Streaming output = actually executed
                    tests: group.tests,
                    failing_tests: group.failures,
                });
            }
        }

        // Parse bazel summary from non-streaming lines
        for line in &non_streaming_lines {
            let text = line.text.trim();
            if let Some(caps) = bazel_summary_re.captures(text) {
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
            }
        }

        return GoTestResult {
            packages,
            bazel_summary,
        };
    }

    // Non-streaming format (original path)
    for line in &non_streaming_lines {
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

struct GoTestGroup {
    tests: Vec<GoTest>,
    failures: Vec<GoFailingTest>,
}

/// Parse Go test output lines for a single target group.
fn parse_go_test_group(lines: &[CleanLine]) -> GoTestGroup {
    let run_re = Regex::new(r"^=== RUN\s+(\S+)").unwrap();
    let result_re = Regex::new(r"^--- (PASS|FAIL|SKIP): (\S+) \(([\d.]+)s\)$").unwrap();

    let mut tests = Vec::new();
    let mut failures = Vec::new();
    let mut current_run_name: Option<String> = None;
    let mut current_run_output: Vec<String> = Vec::new();

    for line in lines {
        let text = line.text.trim();

        if let Some(caps) = run_re.captures(text) {
            current_run_name = Some(caps[1].to_string());
            current_run_output.clear();
            continue;
        }

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

            tests.push(GoTest {
                name: name.clone(),
                status: status.clone(),
                duration_secs: duration,
            });

            if status == GoTestStatus::Fail {
                let output = if current_run_name.as_deref() == Some(&name) {
                    std::mem::take(&mut current_run_output)
                } else {
                    Vec::new()
                };
                failures.push(GoFailingTest {
                    name,
                    duration_secs: duration,
                    output,
                });
            }

            current_run_name = None;
            current_run_output.clear();
            continue;
        }

        // Collect output lines for failure capture
        if current_run_name.is_some() && !text.is_empty() && !text.starts_with("=== ") {
            current_run_output.push(text.to_string());
        }
    }

    GoTestGroup { tests, failures }
}

fn flush_package(
    packages: &mut Vec<GoTestPackage>,
    target: &mut Option<String>,
    tests: &mut Vec<GoTest>,
    failures: &mut Vec<GoFailingTest>,
) {
    if let Some(target) = target.take() {
        // Skip packages with no test output (Bazel cache hits)
        if tests.is_empty() && failures.is_empty() {
            return;
        }
        let passed = failures.is_empty();
        packages.push(GoTestPackage {
            target,
            passed,
            executed: false, // Set later based on Bazel progress line durations
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

    #[test]
    fn test_parse_streaming_format() {
        let input = lines(&[
            "@@//services/foo/itest:scenario_test | === RUN   TestHealth",
            "@@//services/foo/itest:scenario_test | --- PASS: TestHealth (0.01s)",
            "@@//services/foo/itest:scenario_test | === RUN   TestCreate",
            "@@//services/foo/itest:scenario_test |     scenario_test.go:42: expected ok",
            "@@//services/foo/itest:scenario_test | --- FAIL: TestCreate (0.02s)",
            "@@//services/foo/itest:sboxd | {\"level\":\"INFO\",\"msg\":\"server log\"}",
            "Executed 1 out of 1 tests: 0 tests pass and 1 fail locally.",
        ]);
        let result = parse_go_tests(&input);
        assert_eq!(result.packages.len(), 1);
        let pkg = &result.packages[0];
        assert_eq!(pkg.target, "//services/foo/itest");
        assert!(!pkg.passed);
        assert_eq!(pkg.tests.len(), 2);
        assert_eq!(pkg.failing_tests.len(), 1);
        assert_eq!(pkg.failing_tests[0].name, "TestCreate");
        assert_eq!(pkg.failing_tests[0].output.len(), 1);
        assert!(pkg.failing_tests[0].output[0].contains("expected ok"));
        let summary = result.bazel_summary.unwrap();
        assert_eq!(summary.failed, 1);
    }
}
