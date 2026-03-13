pub mod golint;
pub mod gotest;
pub mod mocha;
pub mod nextest;
pub mod script_error;

use crate::log_parser::CleanLine;
use serde::Serialize;

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
