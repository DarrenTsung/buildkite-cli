pub mod mocha;
pub mod nextest;

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
    #[serde(rename = "unknown")]
    Unknown { line_count: usize },
}

pub fn classify(job_name: &str) -> Box<dyn JobParser> {
    if job_name.contains("rust-tests") || job_name.contains("nextest") {
        Box::new(nextest::NextestParser)
    } else if job_name.contains("typescript-tests") || job_name.contains("mocha") {
        Box::new(mocha::MochaParser)
    } else {
        Box::new(UnknownParser)
    }
}

struct UnknownParser;

impl JobParser for UnknownParser {
    fn parse(&self, lines: &[CleanLine]) -> JobResult {
        JobResult::Unknown {
            line_count: lines.len(),
        }
    }
}
