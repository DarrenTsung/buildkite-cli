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
    // Go compiler errors: file.go:line:col: message
    // Matches full paths (services/foo/bar.go:10:5: ...) and short paths (bar.go:10:5: ...)
    let go_error_re = Regex::new(r"^\S+\.go:\d+:\d+:\s+").unwrap();

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
            || go_error_re.is_match(text)
        {
            errors.push(text.to_string());
            continue;
        }

        // Capture context lines right after an error
        if !errors.is_empty() {
            let last = errors.last().unwrap();
            // TypeScript error context: source code and underline markers
            if ts_error_re.is_match(last) || last.ends_with("~~") {
                if text.contains('~') || text.starts_with(|c: char| c.is_ascii_digit()) {
                    errors.push(text.to_string());
                    continue;
                }
            }
            // Go compiler error context: have/want type mismatch lines
            if go_error_re.is_match(last)
                || last.starts_with("have ")
                || last.starts_with("want ")
            {
                if text.starts_with("have ") || text.starts_with("want ") {
                    errors.push(text.to_string());
                    continue;
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
    fn test_parse_go_compile_errors() {
        let input = lines(&[
            "ERROR: /home/circleci/figma/services/agentplat/sbox/sboxd/server/BUILD.bazel:37:8: GoCompilePkg services/agentplat/sbox/sboxd/server/server_test.internal.a failed: (Exit 1): builder failed: error executing GoCompilePkg command",
            "Use --sandbox_debug to see verbose messages from the sandbox",
            "services/agentplat/sbox/sboxd/server/server_test.go:710:25: undefined: WithDefaultWorkspacePath",
            "services/agentplat/sbox/sboxd/server/server_test.go:719:52: too many arguments in call to wsMgr.CreateWorkspaceForBootstrap",
            "\thave (context.Context, \"figma.com/services/agentplat/proto\".WorkspaceID, string)",
            "\twant (context.Context, \"figma.com/services/agentplat/proto\".WorkspaceID)",
            "compilepkg: error running subcommand external/rules_go++go_sdk+go_sdk/pkg/tool/linux_amd64/compile: exit status 2",
            "ERROR: Build did NOT complete successfully",
            "FAILED: ",
            "exit status 1",
        ]);
        let result = parse_script_errors(&input);
        // Should capture the ERROR lines and Go compiler errors
        let errors_str = result.errors.join("\n");
        assert!(
            errors_str.contains("undefined: WithDefaultWorkspacePath"),
            "should capture undefined error"
        );
        assert!(
            errors_str.contains("too many arguments"),
            "should capture too-many-arguments error"
        );
        assert!(
            errors_str.contains("have (context.Context"),
            "should capture have/want context"
        );
        assert!(
            errors_str.contains("want (context.Context"),
            "should capture have/want context"
        );
        // First exit status found is 2 (from the Go compiler)
        assert_eq!(result.exit_code, Some(2));
    }

    #[test]
    fn test_parse_go_compile_errors_short_paths() {
        // golangci-lint produces shorter paths without the full package prefix
        let input = lines(&[
            "sboxd/server/server_test.go:710:25: undefined: WithDefaultWorkspacePath",
            "sboxd/server/server_test.go:719:52: too many arguments in call to wsMgr.CreateWorkspaceForBootstrap",
            "\thave (context.Context, \"figma.com/services/agentplat/proto\".WorkspaceID, string)",
            "\twant (context.Context, \"figma.com/services/agentplat/proto\".WorkspaceID)",
            "exit status 1",
        ]);
        let result = parse_script_errors(&input);
        assert_eq!(
            result.errors.len(),
            4,
            "should capture 2 errors + 2 context lines, got: {:?}",
            result.errors
        );
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
