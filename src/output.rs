use crate::jobs::JobResult;
use anyhow::Result;
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
        JobResult::Unknown { line_count } => {
            out.push_str(&format!(
                "\nUnknown job type. Cleaned log has {} lines.\n",
                line_count
            ));
        }
    }

    out
}
