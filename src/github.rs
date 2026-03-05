use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Command;

pub struct PrCheck {
    pub name: String,
    pub state: CheckState,
    pub link: String,
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

pub fn fetch_pr_checks() -> Result<PrInfo> {
    // Get PR number and branch name
    let view_output = Command::new("gh")
        .args(["pr", "view", "--json", "number,headRefName"])
        .output()
        .context("Failed to run `gh pr view`")?;

    if !view_output.status.success() {
        let stderr = String::from_utf8_lossy(&view_output.stderr);
        anyhow::bail!("gh pr view failed: {}", stderr.trim());
    }

    let pr_view: GhPrView =
        serde_json::from_slice(&view_output.stdout).context("Failed to parse gh pr view JSON")?;

    // Get all checks
    let checks_output = Command::new("gh")
        .args(["pr", "checks", "--json", "name,state,link,bucket"])
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
        })
        .collect();

    Ok(PrInfo {
        number: pr_view.number,
        head_branch: pr_view.head_ref_name,
        checks,
    })
}
