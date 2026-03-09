use crate::jobs::{JobParser, JobResult};
use crate::log_parser::CleanLine;
use regex::Regex;
use serde::Serialize;

pub struct GoLintParser;

#[derive(Debug, Serialize)]
pub struct GoLintResult {
    pub issues: Vec<LintIssue>,
}

#[derive(Debug, Serialize)]
pub struct LintIssue {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub message: String,
    pub linter: String,
}

impl JobParser for GoLintParser {
    fn parse(&self, lines: &[CleanLine]) -> JobResult {
        let result = parse_golint(lines);
        JobResult::GoLint(result)
    }
}

fn parse_golint(lines: &[CleanLine]) -> GoLintResult {
    // golangci-lint output: path/to/file.go:20:2: message (linter_name)
    let issue_re =
        Regex::new(r"^(\S+\.go):(\d+):(\d+):\s+(.+)\s+\((\w+)\)$").unwrap();

    let mut issues = Vec::new();

    for line in lines {
        let text = line.text.trim();
        if let Some(caps) = issue_re.captures(text) {
            issues.push(LintIssue {
                file: caps[1].to_string(),
                line: caps[2].parse().unwrap_or(0),
                col: caps[3].parse().unwrap_or(0),
                message: caps[4].to_string(),
                linter: caps[5].to_string(),
            });
        }
    }

    GoLintResult { issues }
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
    fn test_parse_lint_issue() {
        let input = lines(&[
            "INFO golangci-lint has version unknown",
            "services/agentplat/sbox/sboxd/handler/workspace_handler_test.go:20:2: field createCalled is unused (unused)",
            "\tcreateCalled    bool",
            "\t^",
            "1 issues:",
        ]);
        let result = parse_golint(&input);
        assert_eq!(result.issues.len(), 1);
        let issue = &result.issues[0];
        assert_eq!(
            issue.file,
            "services/agentplat/sbox/sboxd/handler/workspace_handler_test.go"
        );
        assert_eq!(issue.line, 20);
        assert_eq!(issue.col, 2);
        assert_eq!(issue.message, "field createCalled is unused");
        assert_eq!(issue.linter, "unused");
    }

    #[test]
    fn test_no_issues() {
        let input = lines(&[
            "INFO golangci-lint has version unknown",
            "INFO [runner] Issues before processing: 0, after processing: 0",
        ]);
        let result = parse_golint(&input);
        assert_eq!(result.issues.len(), 0);
    }
}
