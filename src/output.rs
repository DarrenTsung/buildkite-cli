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
    uncaptured_errors: &[String],
) -> Result<()> {
    // Write JSON (include uncaptured errors alongside the parser result)
    let json_path = output_dir.join(format!("{}_results.json", prefix));
    let json_value = {
        let mut v = serde_json::to_value(result)?;
        if !uncaptured_errors.is_empty() {
            v["uncaptured_errors"] = serde_json::to_value(uncaptured_errors)?;
        }
        v
    };
    let json = serde_json::to_string_pretty(&json_value)?;
    std::fs::write(&json_path, &json)?;
    eprintln!("Results JSON saved to {}", json_path.display());

    // Generate and write summary
    let summary = format_summary(build_number, job_id, job_name, result, uncaptured_errors);
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

    // Track which build URLs have had their job details displayed,
    // so we only show the full job list once per build.
    let mut displayed_builds: std::collections::HashSet<String> = std::collections::HashSet::new();

    for check in &info.checks {
        let has_bk_jobs = !check.bk_steps.is_empty();

        // Derive check-level state from the actual Buildkite jobs when available.
        let effective_state = if has_bk_jobs {
            effective_check_state(&check.bk_steps)
        } else {
            match check.state {
                CheckState::Passed => EffectiveState::Passed,
                CheckState::Failed => EffectiveState::Failed,
                CheckState::Pending => EffectiveState::Pending,
            }
        };

        let (icon, suffix) = match effective_state {
            EffectiveState::Passed => {
                passed += 1;
                ("✓", String::new())
            }
            EffectiveState::Failed => {
                failed += 1;
                ("✗", String::new())
            }
            EffectiveState::Pending => {
                pending += 1;
                ("→", String::new())
            }
        };
        println!("  {} {}{}", icon, check.name, suffix);

        // Only show the full job details once per build URL.
        let build_url_key = check
            .link
            .split('#')
            .next()
            .unwrap_or(&check.link)
            .to_string();
        let show_job_details = has_bk_jobs && displayed_builds.insert(build_url_key);

        if show_job_details {
            let build_url = check
                .link
                .split('#')
                .next()
                .unwrap_or(&check.link);

            // Categorize steps: show failed individually, collapse
            // running by name, collapse passed/waiting into counts.
            let mut passed_count = 0u32;
            let mut soft_failed_count = 0u32;
            let mut waiting_count = 0u32;
            // Running jobs grouped by name for collapsing shards.
            let mut running_groups: Vec<(String, Vec<&crate::github::BkStepSummary>)> = Vec::new();

            for step in &check.bk_steps {
                match step.current_state.as_str() {
                    "passed" if step.failed_attempts == 0 => {
                        passed_count += 1;
                        continue;
                    }
                    "soft_failed" if step.failed_attempts == 0 => {
                        soft_failed_count += 1;
                        continue;
                    }
                    "waiting" | "scheduled" | "assigned" | "accepted" => {
                        waiting_count += 1;
                        continue;
                    }
                    "running" if step.failed_attempts == 0 => {
                        // Group running jobs by name
                        if let Some(group) = running_groups.iter_mut().find(|(n, _)| n == &step.name)
                        {
                            group.1.push(step);
                        } else {
                            running_groups.push((step.name.clone(), vec![step]));
                        }
                        continue;
                    }
                    _ => {}
                }

                // Failed, timed_out, canceled, or passed-with-retries: show individually
                let step_icon = match step.current_state.as_str() {
                    "passed" => "✓",
                    "soft_failed" => "~",
                    "failed" | "timed_out" | "canceled" => "✗",
                    "running" => "→",
                    _ => "-",
                };
                let retry_note = if step.failed_attempts > 0 {
                    format!(
                        " ({} prior {} failed)",
                        step.failed_attempts,
                        if step.failed_attempts == 1 {
                            "attempt"
                        } else {
                            "attempts"
                        }
                    )
                } else {
                    String::new()
                };
                let duration_str = step
                    .duration_secs
                    .map(|s| format!(" [{}]", format_duration(s)))
                    .unwrap_or_default();
                println!(
                    "      {} {} ({}){}{}",
                    step_icon, step.name, step.current_state, duration_str, retry_note
                );
                println!(
                    "        {}#{}",
                    build_url, step.job_id
                );
                for (i, prior_id) in step.prior_job_ids.iter().enumerate() {
                    let attempt_num = step.failed_attempts - i as u32;
                    println!(
                        "        attempt {}: {}#{}",
                        attempt_num, build_url, prior_id
                    );
                }
            }

            // Show running groups (collapsed when >1 shard)
            for (name, steps) in &running_groups {
                if steps.len() == 1 {
                    let step = steps[0];
                    let duration_str = step
                        .duration_secs
                        .map(|s| format!(" [{}]", format_duration(s)))
                        .unwrap_or_default();
                    println!("      → {} (running){}", name, duration_str);
                    println!("        {}#{}", build_url, step.job_id);
                } else {
                    // Show each shard individually so every job has a clickable URL.
                    for step in steps {
                        let duration_str = step
                            .duration_secs
                            .map(|s| format!(" [{}]", format_duration(s)))
                            .unwrap_or_default();
                        println!("      → {} (running){}", name, duration_str);
                        println!("        {}#{}", build_url, step.job_id);
                    }
                }
            }

            // Collapsed summaries
            let mut collapsed = Vec::new();
            if passed_count > 0 {
                collapsed.push(format!("{} passed", passed_count));
            }
            if soft_failed_count > 0 {
                collapsed.push(format!("{} soft-failed", soft_failed_count));
            }
            if waiting_count > 0 {
                collapsed.push(format!("{} waiting", waiting_count));
            }
            if !collapsed.is_empty() {
                println!("      ({})", collapsed.join(", "));
            }
        } else if !matches!(check.state, CheckState::Passed) {
            // No Buildkite expansion available, just show the link
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

enum EffectiveState {
    Passed,
    Failed,
    Pending,
}

/// Derive the overall check state from its Buildkite jobs.
/// `soft_failed` is treated as passing (Buildkite considers it non-blocking).
fn effective_check_state(steps: &[crate::github::BkStepSummary]) -> EffectiveState {
    let any_failed = steps.iter().any(|s| {
        matches!(
            s.current_state.as_str(),
            "failed" | "timed_out" | "canceled"
        )
    });
    if any_failed {
        return EffectiveState::Failed;
    }
    let all_passed = steps
        .iter()
        .all(|s| matches!(s.current_state.as_str(), "passed" | "soft_failed"));
    if all_passed {
        EffectiveState::Passed
    } else {
        EffectiveState::Pending
    }
}


fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
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
    uncaptured_errors: &[String],
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

            let executed: Vec<_> = gotest.packages.iter().filter(|p| p.executed).collect();
            let cached_count = gotest.packages.len() - executed.len();

            if !executed.is_empty() {
                out.push_str(&format!("\nExecuted ({}):\n", executed.len()));
                for pkg in &executed {
                    let icon = if pkg.passed { "✓" } else { "✗" };
                    out.push_str(&format!(
                        "  {} {} ({} tests)\n",
                        icon,
                        pkg.target,
                        pkg.tests.len()
                    ));
                }
            }
            if cached_count > 0 {
                out.push_str(&format!("\n{} cached targets passed\n", cached_count));
            }

            if !failing_pkgs.is_empty() {
                for pkg in &failing_pkgs {
                    out.push_str(&format!("\n=== {} ===\n", pkg.target));

                    if !pkg.failing_tests.is_empty() {
                        // Go test failures with structured output
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
                    } else if !pkg.raw_output.is_empty() {
                        // Non-Go target failure: show error lines only,
                        // full output is in the clean log file.
                        out.push_str("FAILED (non-Go target), error lines:\n");
                        for line in &pkg.raw_output {
                            out.push_str(&format!("  {}\n", line));
                        }
                        out.push_str("  (full output in *_clean.log)\n");
                    }
                }
            }
        }
        JobResult::GoLint(golint) => {
            if golint.issues.is_empty() {
                out.push_str("\nNo lint issues found.\n");
            } else {
                out.push_str(&format!("\n{} lint issues:\n", golint.issues.len()));
                for issue in &golint.issues {
                    out.push_str(&format!(
                        "\n  {}:{}:{}\n    {} ({})\n",
                        issue.file, issue.line, issue.col, issue.message, issue.linter
                    ));
                }
            }
        }
        JobResult::ScriptError(script) => {
            if let Some(ref cmd) = script.failed_command {
                out.push_str(&format!("\nFailed command: {}\n", cmd));
            }
            if let Some(code) = script.exit_code {
                out.push_str(&format!("Exit code: {}\n", code));
            }
            if script.errors.is_empty() {
                out.push_str("\nNo specific errors found in log.\n");
            } else {
                out.push_str(&format!("\n{} errors:\n", script.errors.len()));
                for err in &script.errors {
                    out.push_str(&format!("  {}\n", err));
                }
            }
        }
    }

    if !uncaptured_errors.is_empty() {
        out.push_str(&format!(
            "\n⚠ {} UNCAPTURED ERROR{}:\n",
            uncaptured_errors.len(),
            if uncaptured_errors.len() == 1 { "" } else { "S" }
        ));
        out.push_str("(error-like lines the parser did not capture)\n");
        for err in uncaptured_errors {
            out.push_str(&format!("  {}\n", err));
        }
    }

    out
}
