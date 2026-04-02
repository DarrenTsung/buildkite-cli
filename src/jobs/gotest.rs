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
    /// Raw output for non-Go test targets (e.g. TS itests) where we can't
    /// parse structured test results but need to surface the error output.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub raw_output: Vec<String>,
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
        let mut failed_targets = Vec::new();

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
                    executed: true,
                    tests: group.tests,
                    failing_tests: group.failures,
                    raw_output: Vec::new(),
                });
            } else if !content_lines.is_empty() {
                // Non-Go target with output but no Go test patterns.
                // Check if it looks like a failure (error lines, non-zero
                // exit, etc.) and include it with raw output so failures
                // aren't silently dropped.
                let has_errors = content_lines.iter().any(|l| line_looks_like_error(l));
                if has_errors {
                    packages.push(GoTestPackage {
                        target: target.clone(),
                        passed: false,
                        executed: true,
                        tests: Vec::new(),
                        failing_tests: Vec::new(),
                        raw_output: extract_error_lines(content_lines),
                    });
                }
            }
        }

        // Parse bazel summary and FAILED/TIMEOUT lines from non-streaming lines
        let bazel_failed_re =
            Regex::new(r"^(//\S+)\s+(FAILED|TIMEOUT)(?:\s+in\s+([\d.]+)s)?").unwrap();

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
            if let Some(caps) = bazel_failed_re.captures(text) {
                failed_targets.push(caps[1].to_string());
            }
        }

        // Add failed targets that weren't already captured by streaming output
        add_missing_failed_targets(&mut packages, &failed_targets);

        return GoTestResult {
            packages,
            bazel_summary,
        };
    }

    // Bazel FAILED/TIMEOUT target lines (appear after test output sections)
    let bazel_failed_re =
        Regex::new(r"^(//\S+)\s+(FAILED|TIMEOUT)(?:\s+in\s+([\d.]+)s)?").unwrap();
    let mut failed_targets = Vec::new();

    // Collect all lines in each "Test output for" section so non-Go
    // failures (e.g. TS itests) aren't silently dropped.
    let mut section_output: Vec<String> = Vec::new();

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
                &mut section_output,
            );
            current_target = Some(caps[1].to_string());
            current_run_name = None;
            current_run_output.clear();
            section_output.clear();
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
                &mut section_output,
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

        // Bazel FAILED/TIMEOUT target lines
        if let Some(caps) = bazel_failed_re.captures(text) {
            failed_targets.push(caps[1].to_string());
            continue;
        }

        // Collect output lines while a test is running (for failure capture)
        if current_run_name.is_some() && !text.is_empty() && !text.starts_with("=== ") {
            current_run_output.push(text.to_string());
        }

        // Also collect all section output for non-Go target detection
        if current_target.is_some() && !text.is_empty() {
            section_output.push(text.to_string());
        }
    }

    // Flush last package
    flush_package(
        &mut packages,
        &mut current_target,
        &mut current_tests,
        &mut current_failures,
        &mut section_output,
    );

    // Add failed targets that weren't captured by "Test output for" sections
    add_missing_failed_targets(&mut packages, &failed_targets);

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
    section_output: &mut Vec<String>,
) {
    if let Some(target) = target.take() {
        if !tests.is_empty() || !failures.is_empty() {
            // Go test output found: use structured results
            let passed = failures.is_empty();
            packages.push(GoTestPackage {
                target,
                passed,
                executed: false, // Set later based on Bazel progress line durations
                tests: std::mem::take(tests),
                failing_tests: std::mem::take(failures),
                raw_output: Vec::new(),
            });
        } else if section_output.iter().any(|l| line_looks_like_error(l)) {
            // Non-Go target with error output (e.g. TS itests).
            // Include it as a failed package with raw output so
            // failures aren't silently hidden.
            packages.push(GoTestPackage {
                target,
                passed: false,
                executed: true,
                tests: Vec::new(),
                failing_tests: Vec::new(),
                raw_output: extract_error_lines(section_output),
            });
        }
        // Otherwise: no tests and no errors = Bazel cache hit, skip.
        tests.clear();
        failures.clear();
        section_output.clear();
    }
}

use super::line_looks_like_error;

/// Extract only error-like lines from raw output for the summary.
/// The full raw output is available in the clean log file.
fn extract_error_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| line_looks_like_error(l))
        .map(|l| l.trim().to_string())
        .collect()
}

/// Add failed Bazel targets that weren't captured by test output sections.
/// These come from `//target FAILED in Xs` lines in the Bazel summary.
fn add_missing_failed_targets(packages: &mut Vec<GoTestPackage>, failed_targets: &[String]) {
    for target in failed_targets {
        // Check if this target (or its package prefix) is already tracked
        let already_tracked = packages.iter().any(|p| {
            p.target == *target
                || target.starts_with(&p.target)
                || (target.contains(':') && {
                    let pkg = &target[..target.rfind(':').unwrap()];
                    p.target == pkg || p.target.starts_with(pkg)
                })
        });
        if !already_tracked {
            packages.push(GoTestPackage {
                target: target.clone(),
                passed: false,
                executed: true,
                tests: Vec::new(),
                failing_tests: Vec::new(),
                raw_output: vec!["(no test output captured, target reported FAILED by Bazel)".to_string()],
            });
        }
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

    #[test]
    fn test_non_go_target_failure_captured() {
        // Simulates a TS itest failing alongside passing Go tests.
        // Before the fix, the TS failure was silently dropped.
        let input = lines(&[
            "==================== Test output for //services/agentplat/sbox/sboxd:sboxd_test:",
            "=== RUN   TestHealth",
            "--- PASS: TestHealth (0.01s)",
            "==================== Test output for //services/agentplat/ts-client/itest:itest:",
            "Error [AgentPlatError]: bootstrap config is required",
            "    at SboxClient.createWorkspace (src/client.ts:42:11)",
            "==================== Test output for //services/agentplat/ts-client/itest:itest_disconnect:",
            "Error [AgentPlatError]: bootstrap config is required",
            "    at SboxClient.createWorkspace (src/client.ts:42:11)",
            "//services/agentplat/ts-client/itest:itest                       FAILED in 12.3s",
            "//services/agentplat/ts-client/itest:itest_disconnect            FAILED in 728.3s",
            "Executed 3 out of 8 tests: 5 tests pass and 3 fail locally.",
        ]);
        let result = parse_go_tests(&input);

        // Should have 3 packages: the passing Go test + 2 failing TS tests
        assert_eq!(result.packages.len(), 3, "expected Go pkg + 2 TS failures, got: {:?}",
            result.packages.iter().map(|p| &p.target).collect::<Vec<_>>());

        let go_pkg = result.packages.iter().find(|p| p.target.contains("sboxd_test")).unwrap();
        assert!(go_pkg.passed);
        assert_eq!(go_pkg.tests.len(), 1);

        let ts_itest = result.packages.iter().find(|p| p.target.ends_with("itest:itest")).unwrap();
        assert!(!ts_itest.passed);
        assert!(ts_itest.tests.is_empty(), "non-Go target should have no Go tests");
        assert!(!ts_itest.raw_output.is_empty(), "should have raw error output");
        assert!(ts_itest.raw_output.iter().any(|l| l.contains("bootstrap config is required")));

        let summary = result.bazel_summary.unwrap();
        assert_eq!(summary.failed, 3);
    }

    #[test]
    fn test_bazel_failed_target_without_output_section() {
        // A target that appears in FAILED lines but has no "Test output for" section
        let input = lines(&[
            "==================== Test output for //pkg:pkg_test:",
            "=== RUN   TestFoo",
            "--- PASS: TestFoo (0.01s)",
            "//services/ts/itest:itest   FAILED in 5.0s",
            "Executed 2 out of 2 tests: 1 tests pass and 1 fail locally.",
        ]);
        let result = parse_go_tests(&input);

        assert_eq!(result.packages.len(), 2);
        let failed = result.packages.iter().find(|p| p.target.contains("ts/itest")).unwrap();
        assert!(!failed.passed);
        assert!(!failed.raw_output.is_empty());
    }

    #[test]
    fn test_streaming_non_go_target_failure() {
        // TS test failure in streaming format alongside Go test
        let input = lines(&[
            "@@//services/foo/itest:scenario_test | === RUN   TestHealth",
            "@@//services/foo/itest:scenario_test | --- PASS: TestHealth (0.01s)",
            "@@//services/ts-client/itest:itest | Error [AgentPlatError]: bootstrap config is required",
            "@@//services/ts-client/itest:itest |     at SboxClient.createWorkspace (src/client.ts:42:11)",
            "//services/ts-client/itest:itest   FAILED in 12.3s",
            "Executed 2 out of 2 tests: 1 tests pass and 1 fail locally.",
        ]);
        let result = parse_go_tests(&input);

        assert_eq!(result.packages.len(), 2);

        let go_pkg = result.packages.iter().find(|p| p.target.contains("foo/itest")).unwrap();
        assert!(go_pkg.passed);

        let ts_pkg = result.packages.iter().find(|p| p.target.contains("ts-client")).unwrap();
        assert!(!ts_pkg.passed);
        assert!(!ts_pkg.raw_output.is_empty());
        assert!(ts_pkg.raw_output.iter().any(|l| l.contains("bootstrap config")));
    }
}
