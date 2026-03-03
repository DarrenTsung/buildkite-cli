use crate::jobs::{JobParser, JobResult};
use crate::log_parser::CleanLine;
use regex::Regex;
use serde::Serialize;

pub struct NextestParser;

#[derive(Debug, Serialize)]
pub struct NextestResult {
    pub runs: Vec<NextestRun>,
}

#[derive(Debug, Serialize)]
pub struct NextestRun {
    pub target: String,
    pub binary: String,
    pub summary: RunSummary,
    pub tests: Vec<TestResult>,
    pub failing_tests: Vec<FailingTest>,
}

#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub skipped: usize,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub name: String,
    pub status: TestStatus,
    pub duration_secs: f64,
}

#[derive(Debug, Serialize)]
pub struct FailingTest {
    pub name: String,
    pub status: TestStatus,
    pub duration_secs: f64,
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TestStatus {
    Pass,
    Fail,
    Timeout,
    Skipped,
}

impl std::fmt::Display for TestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestStatus::Pass => write!(f, "PASS"),
            TestStatus::Fail => write!(f, "FAIL"),
            TestStatus::Timeout => write!(f, "TIMEOUT"),
            TestStatus::Skipped => write!(f, "SKIPPED"),
        }
    }
}

impl JobParser for NextestParser {
    fn parse(&self, lines: &[CleanLine]) -> JobResult {
        let runs = parse_nextest_runs(lines);
        JobResult::Nextest(NextestResult { runs })
    }
}

fn parse_nextest_runs(lines: &[CleanLine]) -> Vec<NextestRun> {
    let nextest_run_re = Regex::new(r"Nextest run ID [0-9a-f-]+ with nextest profile:").unwrap();
    let config_re = Regex::new(
        r"Using nextest config at: .*/bin/(.+?)\.sh\.runfiles/"
    ).unwrap();

    // Find the start indices of each nextest run and their associated config line
    let mut run_starts: Vec<(usize, Option<String>)> = Vec::new();

    // First pass: find config lines and nextest run ID lines
    let mut last_config_target: Option<String> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(caps) = config_re.captures(&line.text) {
            // e.g., "multiplayer/multiplayer/multiplayer_rollout_test_nextest"
            // Convert to "//multiplayer/multiplayer:multiplayer_rollout_test_nextest"
            let path = &caps[1];
            last_config_target = Some(path_to_bazel_target(path));
        }

        if nextest_run_re.is_match(&line.text) {
            run_starts.push((i, last_config_target.take()));
        }
    }

    let mut runs = Vec::new();

    for (idx, (start, target)) in run_starts.iter().enumerate() {
        let end = if idx + 1 < run_starts.len() {
            run_starts[idx + 1].0
        } else {
            lines.len()
        };

        let run_lines = &lines[*start..end];
        if let Some(run) = parse_single_run(run_lines, target.clone()) {
            runs.push(run);
        }
    }

    runs
}

fn path_to_bazel_target(path: &str) -> String {
    // "multiplayer/multiplayer/multiplayer_rollout_test_nextest"
    // -> "//multiplayer/multiplayer:multiplayer_rollout_test_nextest"
    if let Some(last_slash) = path.rfind('/') {
        let package = &path[..last_slash];
        let target_name = &path[last_slash + 1..];
        format!("//{}:{}", package, target_name)
    } else {
        format!("//:{}", path)
    }
}

fn parse_single_run(lines: &[CleanLine], target: Option<String>) -> Option<NextestRun> {
    let test_result_re = Regex::new(
        r"^\s*(PASS|FAIL|TIMEOUT|TERMINATING)\s+\[\s*([\d>]+\.[\d]+)s\]\s+(\S+)\s+(.+)$"
    ).unwrap();
    let summary_re = Regex::new(
        r"Summary\s+\[\s*([\d.]+)s\]\s+(\d+)\s+tests?\s+run:\s+(.*)"
    ).unwrap();
    let stdout_marker = "stdout ───";
    let stderr_marker = "stderr ───";
    let mut tests: Vec<TestResult> = Vec::new();
    let mut failing_tests: Vec<FailingTest> = Vec::new();
    let mut summary = None;
    let mut binary_name = String::new();
    let mut seen_summary = false;

    // Track failure detail blocks
    let mut in_failure_block = false;
    let mut current_failure: Option<FailureBlockState> = None;

    for line in lines {
        let text = &line.text;
        let trimmed = text.trim();

        // Check for failure detail separator (end of failure block)
        if trimmed.starts_with("──────") && in_failure_block {
            if let Some(state) = current_failure.take() {
                failing_tests.push(state.into_failing_test());
            }
            in_failure_block = false;
            continue;
        }

        // If we're inside a failure detail block, collect stdout/stderr
        if in_failure_block {
            if let Some(ref mut state) = current_failure {
                if trimmed.contains(stdout_marker) {
                    state.section = FailureSection::Stdout;
                } else if trimmed.contains(stderr_marker) {
                    state.section = FailureSection::Stderr;
                } else {
                    match state.section {
                        FailureSection::Stdout => state.stdout.push(trimmed.to_string()),
                        FailureSection::Stderr => state.stderr.push(trimmed.to_string()),
                        FailureSection::None => {}
                    }
                }
            }
            continue;
        }

        // Check for test result lines (skip post-summary recaps)
        if let Some(caps) = test_result_re.captures(trimmed) {
            if seen_summary {
                continue;
            }
            let status_str = &caps[1];
            let duration: f64 = caps[2].trim_start_matches('>').parse().unwrap_or(0.0);
            let binary = &caps[3];
            let test_name = caps[4].trim().to_string();

            if binary_name.is_empty() {
                binary_name = binary.to_string();
            }

            let status = match status_str {
                "PASS" => TestStatus::Pass,
                "FAIL" => TestStatus::Fail,
                "TIMEOUT" => TestStatus::Timeout,
                "TERMINATING" => {
                    // TERMINATING is a precursor to TIMEOUT, skip it to avoid double-counting
                    continue;
                }
                _ => continue,
            };

            // If this is in the post-summary section (failure recap), check if we
            // should start a failure detail block
            if status != TestStatus::Pass && status != TestStatus::Skipped {
                // Check if the next lines form a failure detail block
                // by looking for stdout/stderr markers
                tests.push(TestResult {
                    name: test_name.clone(),
                    status: status.clone(),
                    duration_secs: duration,
                });

                // Start tracking a potential failure block
                in_failure_block = true;
                current_failure = Some(FailureBlockState {
                    name: test_name,
                    status,
                    duration_secs: duration,
                    section: FailureSection::None,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                });
            } else {
                tests.push(TestResult {
                    name: test_name,
                    status,
                    duration_secs: duration,
                });
            }
            continue;
        }

        // Check for summary line
        if let Some(caps) = summary_re.captures(trimmed) {
            let duration: f64 = caps[1].parse().unwrap_or(0.0);
            let total: usize = caps[2].parse().unwrap_or(0);
            let rest = &caps[3];

            let passed = extract_count(rest, "passed");
            let failed = extract_count(rest, "failed");
            let timed_out = extract_count(rest, "timed out");
            let skipped = extract_count(rest, "skipped");

            seen_summary = true;
            summary = Some(RunSummary {
                total,
                passed,
                failed,
                timed_out,
                skipped,
                duration_secs: duration,
            });
        }
    }

    // Flush any pending failure block
    if let Some(state) = current_failure.take() {
        failing_tests.push(state.into_failing_test());
    }

    let target = target.unwrap_or_else(|| format!("//:{}", binary_name));

    Some(NextestRun {
        target,
        binary: binary_name,
        summary: summary.unwrap_or(RunSummary {
            total: tests.len(),
            passed: tests.iter().filter(|t| t.status == TestStatus::Pass).count(),
            failed: tests.iter().filter(|t| t.status == TestStatus::Fail).count(),
            timed_out: tests.iter().filter(|t| t.status == TestStatus::Timeout).count(),
            skipped: tests.iter().filter(|t| t.status == TestStatus::Skipped).count(),
            duration_secs: 0.0,
        }),
        tests,
        failing_tests,
    })
}

fn extract_count(s: &str, label: &str) -> usize {
    let pattern = format!(r"(\d+)\s+{}", regex::escape(label));
    Regex::new(&pattern)
        .ok()
        .and_then(|re| re.captures(s))
        .and_then(|caps| caps[1].parse().ok())
        .unwrap_or(0)
}

#[derive(Debug)]
enum FailureSection {
    None,
    Stdout,
    Stderr,
}

#[derive(Debug)]
struct FailureBlockState {
    name: String,
    status: TestStatus,
    duration_secs: f64,
    section: FailureSection,
    stdout: Vec<String>,
    stderr: Vec<String>,
}

impl FailureBlockState {
    fn into_failing_test(self) -> FailingTest {
        FailingTest {
            name: self.name,
            status: self.status,
            duration_secs: self.duration_secs,
            stdout: self.stdout,
            stderr: self.stderr,
        }
    }
}
