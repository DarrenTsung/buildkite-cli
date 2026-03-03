use crate::jobs::{JobParser, JobResult};
use crate::log_parser::CleanLine;
use regex::Regex;
use serde::Serialize;

pub struct MochaParser;

#[derive(Debug, Serialize)]
pub struct MochaResult {
    pub passing: usize,
    pub pending: usize,
    pub failing: usize,
    pub duration: String,
    pub failures: Vec<MochaFailure>,
    pub suppressed_flakes: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct MochaFailure {
    pub number: usize,
    pub suite: String,
    pub test: String,
    pub error_message: String,
    pub stack_trace: Vec<String>,
    pub diff: Option<MochaDiff>,
}

#[derive(Debug, Serialize)]
pub struct MochaDiff {
    pub expected: String,
    pub actual: String,
}

impl JobParser for MochaParser {
    fn parse(&self, lines: &[CleanLine]) -> JobResult {
        JobResult::Mocha(parse_mocha(lines))
    }
}

fn parse_mocha(lines: &[CleanLine]) -> MochaResult {
    let summary_passing_re = Regex::new(r"^\s*(\d+) passing \((.+)\)").unwrap();
    let summary_pending_re = Regex::new(r"^\s*(\d+) pending").unwrap();
    let summary_failing_re = Regex::new(r"^\s*(\d+) failing").unwrap();
    let failure_num_re = Regex::new(r"^\s*(\d+)\) (.+)").unwrap();
    let suppressed_re =
        Regex::new(r"(\d+) of these were suppressed known flakes").unwrap();

    let mut passing = 0;
    let mut pending = 0;
    let mut failing = 0;
    let mut duration = String::new();
    let mut suppressed_flakes = None;

    // First pass: find summary lines
    let mut summary_line = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.text.trim();
        if let Some(caps) = summary_passing_re.captures(trimmed) {
            passing = caps[1].parse().unwrap_or(0);
            duration = caps[2].to_string();
            summary_line = Some(i);
        }
        if let Some(caps) = summary_pending_re.captures(trimmed) {
            pending = caps[1].parse().unwrap_or(0);
        }
        if let Some(caps) = summary_failing_re.captures(trimmed) {
            failing = caps[1].parse().unwrap_or(0);
        }
        if let Some(caps) = suppressed_re.captures(trimmed) {
            suppressed_flakes = caps[1].parse().ok();
        }
    }

    // Second pass: parse failure blocks (they appear after the summary)
    let mut failures = Vec::new();
    if let Some(start) = summary_line {
        let failure_lines = &lines[start..];
        let mut i = 0;
        while i < failure_lines.len() {
            let trimmed = failure_lines[i].text.trim();
            if let Some(caps) = failure_num_re.captures(trimmed) {
                let number: usize = caps[1].parse().unwrap_or(0);
                let suite = caps[2].to_string();
                i += 1;

                // Next line is the test name (indented further)
                let test = if i < failure_lines.len() {
                    let t = failure_lines[i].text.trim();
                    // Could be "test name:" or just the error directly
                    if t.contains("Error") || t.starts_with("at ") {
                        String::new()
                    } else {
                        i += 1;
                        t.trim_end_matches(':').to_string()
                    }
                } else {
                    String::new()
                };

                // Collect error message and stack trace
                let mut error_message = String::new();
                let mut stack_trace = Vec::new();
                let mut diff = None;
                let mut expected = None;
                let mut actual = None;

                while i < failure_lines.len() {
                    let t = failure_lines[i].text.trim();

                    // Stop at the next failure number or known end markers
                    if failure_num_re.is_match(t) {
                        break;
                    }
                    if t.starts_with("FAIL:") || t.starts_with("INFO:") {
                        break;
                    }

                    if t.starts_with("at ") {
                        stack_trace.push(t.to_string());
                    } else if t == "+ expected - actual" {
                        // diff section follows
                    } else if t.starts_with('+') && expected.is_none() {
                        expected = Some(t.trim_start_matches('+').to_string());
                    } else if t.starts_with('-') && actual.is_none() {
                        actual = Some(t.trim_start_matches('-').to_string());
                    } else if error_message.is_empty()
                        && !t.is_empty()
                        && !t.starts_with("at ")
                    {
                        error_message = t.to_string();
                    }

                    i += 1;
                }

                if let (Some(exp), Some(act)) = (expected, actual) {
                    diff = Some(MochaDiff {
                        expected: exp,
                        actual: act,
                    });
                }

                failures.push(MochaFailure {
                    number,
                    suite,
                    test,
                    error_message,
                    stack_trace,
                    diff,
                });
                continue;
            }
            i += 1;
        }
    }

    MochaResult {
        passing,
        pending,
        failing,
        duration,
        failures,
        suppressed_flakes,
    }
}
