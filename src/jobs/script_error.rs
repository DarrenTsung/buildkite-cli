use crate::jobs::{JobParser, JobResult};
use crate::log_parser::CleanLine;
use regex::Regex;
use serde::Serialize;

pub struct ScriptErrorParser;

#[derive(Debug, Serialize)]
pub struct ScriptErrorResult {
    pub errors: Vec<String>,
    pub failed_command: Option<String>,
    pub exit_code: Option<i32>,
}

impl JobParser for ScriptErrorParser {
    fn parse(&self, lines: &[CleanLine]) -> JobResult {
        let result = parse_script_errors(lines);
        JobResult::ScriptError(result)
    }
}

fn parse_script_errors(lines: &[CleanLine]) -> ScriptErrorResult {
    let error_re = Regex::new(r"(?i)^ERROR:|^error[ :]|^\s*error TS\d+:").unwrap();
    let exit_re = Regex::new(r"exit status (\d+)").unwrap();
    let failed_cmd_re =
        Regex::new(r"ci-interp:.*failed to evaluate steps: command `(.+)` failed").unwrap();
    // Lines starting with a cross mark indicate a check failure
    let cross_re = Regex::new(r"^❌").unwrap();
    // TypeScript error lines: file.ts:line:col - error TSxxxx: message
    let ts_error_re = Regex::new(r"^\S+\.tsx?:\d+:\d+ - error TS\d+:").unwrap();
    // Bazel FAILED targets
    let bazel_failed_re = Regex::new(r"^//\S+\s+FAILED").unwrap();

    let mut errors = Vec::new();
    let mut failed_command = None;
    let mut exit_code = None;

    // CI noise patterns to skip
    let noise_re = Regex::new(
        r"(?i)artifact|upload|docker|datadog|buildkite|ci-interp: reaping|hooks/|ssh|revoking|pruning|disk snapshot|reclaim|ci-cache|retry-cli|test-processor|git trace|check-feature-flag.*checking"
    )
    .unwrap();

    for line in lines {
        let text = line.text.trim();

        if let Some(caps) = failed_cmd_re.captures(text) {
            failed_command = Some(caps[1].to_string());
            continue;
        }

        if exit_code.is_none() {
            if let Some(caps) = exit_re.captures(text) {
                exit_code = caps[1].parse().ok();
            }
        }

        // Skip CI infrastructure noise
        if noise_re.is_match(text) {
            continue;
        }

        if error_re.is_match(text)
            || cross_re.is_match(text)
            || ts_error_re.is_match(text)
            || bazel_failed_re.is_match(text)
        {
            errors.push(text.to_string());
            continue;
        }

        // Capture context lines right after a TypeScript error (the source code line)
        if !errors.is_empty() {
            let last = errors.last().unwrap();
            if ts_error_re.is_match(last) || last.ends_with("~~") {
                // Source code or underline context
                if text.contains('~') || text.starts_with(|c: char| c.is_ascii_digit()) {
                    errors.push(text.to_string());
                }
            }
        }
    }

    ScriptErrorResult {
        errors,
        failed_command,
        exit_code,
    }
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
    fn test_parse_typescript_errors() {
        let input = lines(&[
            "ERROR: /home/circleci/figma/services/cortex/BUILD.bazel:45:11: tsc failed",
            "services/cortex/lib/foo.ts:20:33 - error TS2307: Cannot find module 'bar'",
            "20 import { Foo } from 'bar'",
            "                       ~~~",
            "Found 1 error.",
            "ERROR: Build did NOT complete successfully",
            "exit status 1",
        ]);
        let result = parse_script_errors(&input);
        assert!(result.errors.len() >= 2);
        assert_eq!(result.exit_code, Some(1));
    }

    #[test]
    fn test_parse_feature_flag_error() {
        let input = lines(&[
            "❌ Cortex gate usage: my_flag is not found in feature_flags.json",
            "Total feature flags/gates: 1",
            "exit status 1",
            "2026/03/13 21:59:20 ci-interp: failed to evaluate steps: command `Get git diff and find feature flags` failed with exit code 1",
        ]);
        let result = parse_script_errors(&input);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains("my_flag"));
        assert_eq!(
            result.failed_command.as_deref(),
            Some("Get git diff and find feature flags")
        );
        assert_eq!(result.exit_code, Some(1));
    }
}
