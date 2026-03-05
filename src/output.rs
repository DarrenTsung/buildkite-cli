use crate::buildkite::{JobInfo, ParsedUrl};
use crate::github::{CheckState, PrInfo};
use crate::jobs::JobResult;
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

pub fn write_results(
    output_dir: &Path,
    prefix: &str,
    build_number: &str,
    job_id: &str,
    job_name: &str,
    result: &JobResult,
) -> Result<()> {
    // Write JSON
    let json_path = output_dir.join(format!("{}_results.json", prefix));
    let json = serde_json::to_string_pretty(result)?;
    std::fs::write(&json_path, &json)?;
    eprintln!("Results JSON saved to {}", json_path.display());

    // Generate and write summary
    let summary = format_summary(build_number, job_id, job_name, result);
    let summary_path = output_dir.join(format!("{}_summary.txt", prefix));
    std::fs::write(&summary_path, &summary)?;
    eprintln!("Summary saved to {}", summary_path.display());

    // Print summary to stdout
    println!("{}", summary);

    Ok(())
}

pub fn print_pr_checks(info: &PrInfo) {
    println!("PR #{}: {}\n", info.number, info.head_branch);

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut pending = 0u32;

    for check in &info.checks {
        let (icon, suffix) = match check.state {
            CheckState::Passed => {
                passed += 1;
                ("✓", String::new())
            }
            CheckState::Failed => {
                failed += 1;
                ("✗", " (failed)".to_string())
            }
            CheckState::Pending => {
                pending += 1;
                ("-", " (pending)".to_string())
            }
        };
        println!("  {} {}{}", icon, check.name, suffix);
        if !matches!(check.state, CheckState::Passed) {
            println!("    {}", check.link);
        }
    }

    println!();
    let mut parts = Vec::new();
    if passed > 0 {
        parts.push(format!("{} passed", passed));
    }
    if failed > 0 {
        parts.push(format!("{} failed", failed));
    }
    if pending > 0 {
        parts.push(format!("{} pending", pending));
    }
    println!("{}", parts.join(", "));
}

#[derive(Serialize)]
struct JobJson {
    name: String,
    id: String,
    state: String,
    url: String,
}

pub fn print_build_jobs_json(parsed: &ParsedUrl, jobs: &[JobInfo]) -> Result<()> {
    let base_url = format!(
        "https://buildkite.com/{}/{}/builds/{}",
        parsed.org, parsed.pipeline, parsed.build_number
    );
    let entries: Vec<JobJson> = jobs
        .iter()
        .map(|j| JobJson {
            name: j.name.clone(),
            id: j.id.clone(),
            state: j.state.clone(),
            url: format!("{}#{}", base_url, j.id),
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&entries)?);
    Ok(())
}

pub fn print_build_jobs(build_number: &str, pipeline: &str, base_url: &str, jobs: &[JobInfo]) {
    println!("Build {} ({})\n", build_number, pipeline);
    for job in jobs {
        let icon = match job.state.as_str() {
            "passed" => "✓",
            "failed" | "timed_out" => "✗",
            _ => "-",
        };
        let suffix = match job.state.as_str() {
            "passed" => String::new(),
            other => format!(" ({})", other),
        };
        println!("  {} {}{}", icon, job.name, suffix);
        // Show job URL dimmed on next line for easy copy-paste
        println!("    \x1b[2m{}#{}\x1b[0m", base_url, job.id);
    }
}

fn format_summary(
    build_number: &str,
    job_id: &str,
    job_name: &str,
    result: &JobResult,
) -> String {
    let mut out = String::new();

    out.push_str(&format!("Build: {}\n", build_number));
    out.push_str(&format!("Job: {} ({})\n", job_name, job_id));

    match result {
        JobResult::Nextest(nextest) => {
            for run in &nextest.runs {
                out.push_str(&format!(
                    "\n=== {} ({}) ===\n",
                    run.binary, run.target
                ));

                let s = &run.summary;
                let mut parts = vec![format!("{} passed", s.passed)];
                if s.failed > 0 {
                    parts.push(format!("{} failed", s.failed));
                }
                if s.timed_out > 0 {
                    parts.push(format!("{} timed out", s.timed_out));
                }
                if s.skipped > 0 {
                    parts.push(format!("{} skipped", s.skipped));
                }
                out.push_str(&format!("{} tests: {}\n", s.total, parts.join(", ")));

                if !run.failing_tests.is_empty() {
                    out.push_str("\nFAILURES:\n");
                    for ft in &run.failing_tests {
                        out.push_str(&format!(
                            "  {} [{:.3}s] {}\n",
                            ft.status, ft.duration_secs, ft.name
                        ));

                        if !ft.stdout.is_empty() {
                            out.push_str("    stdout:\n");
                            for line in &ft.stdout {
                                out.push_str(&format!("      {}\n", line));
                            }
                        }
                        if !ft.stderr.is_empty() {
                            out.push_str("    stderr:\n");
                            for line in &ft.stderr {
                                out.push_str(&format!("      {}\n", line));
                            }
                        }
                    }
                }
            }
        }
        JobResult::Mocha(mocha) => {
            out.push_str(&format!(
                "\n{} passing ({}), {} pending, {} failing\n",
                mocha.passing, mocha.duration, mocha.pending, mocha.failing
            ));

            if let Some(suppressed) = mocha.suppressed_flakes {
                out.push_str(&format!(
                    "{} of {} failures suppressed as known flakes\n",
                    suppressed, mocha.failing
                ));
            }

            if !mocha.failures.is_empty() {
                out.push_str("\nFAILURES:\n");
                for f in &mocha.failures {
                    if f.test.is_empty() {
                        out.push_str(&format!("  {}) {}\n", f.number, f.suite));
                    } else {
                        out.push_str(&format!("  {}) {} > {}\n", f.number, f.suite, f.test));
                    }
                    if !f.error_message.is_empty() {
                        out.push_str(&format!("     {}\n", f.error_message));
                    }
                    if let Some(ref diff) = f.diff {
                        out.push_str(&format!(
                            "     expected: {}  actual: {}\n",
                            diff.expected, diff.actual
                        ));
                    }
                    for line in &f.stack_trace {
                        out.push_str(&format!("     {}\n", line));
                    }
                    out.push('\n');
                }
            }
        }
        JobResult::GoTest(gotest) => {
            if let Some(ref summary) = gotest.bazel_summary {
                out.push_str(&format!(
                    "\nBazel: executed {} of {} tests, {} passed, {} failed\n",
                    summary.executed, summary.total, summary.passed, summary.failed
                ));
            }

            let failing_pkgs: Vec<&crate::jobs::gotest::GoTestPackage> =
                gotest.packages.iter().filter(|p| !p.passed).collect();

            if failing_pkgs.is_empty() {
                out.push_str(&format!(
                    "\nAll {} packages passed.\n",
                    gotest.packages.len()
                ));
            } else {
                for pkg in &failing_pkgs {
                    out.push_str(&format!("\n=== {} ===\n", pkg.target));
                    let passed = pkg
                        .tests
                        .iter()
                        .filter(|t| t.status == crate::jobs::gotest::GoTestStatus::Pass)
                        .count();
                    let failed = pkg.failing_tests.len();
                    out.push_str(&format!("{} passed, {} failed\n", passed, failed));

                    out.push_str("\nFAILURES:\n");
                    for ft in &pkg.failing_tests {
                        out.push_str(&format!(
                            "  FAIL [{:.3}s] {}\n",
                            ft.duration_secs, ft.name
                        ));
                        for line in &ft.output {
                            out.push_str(&format!("    {}\n", line));
                        }
                    }
                }
            }
        }
        JobResult::Unknown { line_count } => {
            out.push_str(&format!(
                "\nUnknown job type. Cleaned log has {} lines.\n",
                line_count
            ));
        }
    }

    out
}
