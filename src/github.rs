use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Command;

pub struct PrCheck {
    pub name: String,
    pub state: CheckState,
    pub link: String,
    /// Buildkite step summaries for this check (populated by enrichment).
    pub bk_steps: Vec<BkStepSummary>,
}

/// Summary of a Buildkite pipeline step, aggregated across retry attempts.
pub struct BkStepSummary {
    pub name: String,
    /// State of the most recent attempt (e.g. "failed", "running").
    pub current_state: String,
    /// Number of previous attempts that failed before the current one.
    pub failed_attempts: u32,
    /// Job IDs of prior attempts (most recent first), for log downloads.
    /// The Buildkite REST API only exposes the immediate predecessor, so
    /// this typically has at most 1 entry even if failed_attempts > 1.
    pub prior_job_ids: Vec<String>,
    /// Duration of the current attempt in seconds, or elapsed time if
    /// still running.
    pub duration_secs: Option<u64>,
    /// Job ID for the current attempt.
    pub job_id: String,
}

pub enum CheckState {
    Passed,
    Failed,
    Pending,
}

pub struct PrInfo {
    pub number: u64,
    pub head_branch: String,
    pub checks: Vec<PrCheck>,
}

#[derive(Deserialize)]
struct GhPrView {
    number: u64,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
}

#[derive(Deserialize)]
struct GhCheck {
    name: String,
    bucket: String,
    link: String,
}

pub fn fetch_pr_checks(branch: Option<&str>) -> Result<PrInfo> {
    // Get PR number and branch name
    let mut view_cmd = Command::new("gh");
    view_cmd.args(["pr", "view", "--json", "number,headRefName"]);
    if let Some(b) = branch {
        view_cmd.arg(b);
    }
    let view_output = view_cmd.output().context("Failed to run `gh pr view`")?;

    if !view_output.status.success() {
        let stderr = String::from_utf8_lossy(&view_output.stderr);
        anyhow::bail!("gh pr view failed: {}", stderr.trim());
    }

    let pr_view: GhPrView =
        serde_json::from_slice(&view_output.stdout).context("Failed to parse gh pr view JSON")?;

    // Get all checks
    let mut checks_cmd = Command::new("gh");
    checks_cmd.args(["pr", "checks", "--json", "name,state,link,bucket"]);
    if let Some(b) = branch {
        checks_cmd.arg(b);
    }
    let checks_output = checks_cmd
        .output()
        .context("Failed to run `gh pr checks`")?;

    if !checks_output.status.success() {
        let stderr = String::from_utf8_lossy(&checks_output.stderr);
        anyhow::bail!("gh pr checks failed: {}", stderr.trim());
    }

    let all_checks: Vec<GhCheck> = serde_json::from_slice(&checks_output.stdout)
        .context("Failed to parse gh pr checks JSON")?;

    // Filter to Buildkite checks and map state
    let checks = all_checks
        .into_iter()
        .filter(|c| c.link.contains("buildkite.com"))
        .map(|c| PrCheck {
            name: c.name,
            state: match c.bucket.as_str() {
                "pass" => CheckState::Passed,
                "fail" => CheckState::Failed,
                _ => CheckState::Pending,
            },
            link: c.link,
            bk_steps: Vec::new(),
        })
        .collect();

    Ok(PrInfo {
        number: pr_view.number,
        head_branch: pr_view.head_ref_name,
        checks,
    })
}
