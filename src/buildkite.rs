use anyhow::{Context, Result, bail};
use regex::Regex;

#[derive(Debug, Clone)]
pub struct ParsedUrl {
    pub org: String,
    pub pipeline: String,
    pub build_number: String,
    pub job_id: Option<String>,
}

pub fn parse_url(url: &str) -> Result<ParsedUrl> {
    // Matches with or without #job-id fragment
    let re = Regex::new(
        r"https?://buildkite\.com/([^/]+)/([^/]+)/builds/(\d+)(?:#([0-9a-f-]+))?",
    )?;

    let caps = re.captures(url).context("URL does not match expected Buildkite format: https://buildkite.com/<org>/<pipeline>/builds/<number>[#<job-id>]")?;

    Ok(ParsedUrl {
        org: caps[1].to_string(),
        pipeline: caps[2].to_string(),
        build_number: caps[3].to_string(),
        job_id: caps.get(4).map(|m| m.as_str().to_string()),
    })
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct JobInfo {
    pub id: String,
    pub name: String,
    pub step_key: Option<String>,
    pub state: String,
    pub job_type: String,
    /// True if this job was retried (a newer attempt exists).
    pub retried: bool,
    /// Number of automatic/manual retries before this job. The API replaces
    /// retried jobs in the array, so this is the only way to know about
    /// prior failed attempts.
    pub retries_count: u32,
    /// Job ID of the immediate prior attempt (the one this job retried).
    pub retry_source_job_id: Option<String>,
    /// When the job started running (ISO 8601).
    pub started_at: Option<String>,
    /// When the job finished (ISO 8601). None if still running.
    pub finished_at: Option<String>,
}

pub struct Client {
    token: String,
    client: reqwest::blocking::Client,
}

impl Client {
    pub fn new(token: &str) -> Self {
        Self {
            token: token.to_string(),
            client: reqwest::blocking::Client::new(),
        }
    }

    pub fn download_log(&self, parsed: &ParsedUrl) -> Result<String> {
        let job_id = parsed
            .job_id
            .as_deref()
            .context("Job ID is required to download logs (use a URL with #job-id)")?;
        let url = format!(
            "https://api.buildkite.com/v2/organizations/{}/pipelines/{}/builds/{}/jobs/{}/log",
            parsed.org, parsed.pipeline, parsed.build_number, job_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "text/plain")
            .send()
            .context("HTTP request to Buildkite API failed")?;

        if !resp.status().is_success() {
            bail!(
                "Buildkite API returned status {}: {}",
                resp.status(),
                resp.text().unwrap_or_default()
            );
        }

        resp.text().context("Failed to read response body")
    }

    pub fn fetch_build_jobs(&self, parsed: &ParsedUrl) -> Result<Vec<JobInfo>> {
        let url = format!(
            "https://api.buildkite.com/v2/organizations/{}/pipelines/{}/builds/{}",
            parsed.org, parsed.pipeline, parsed.build_number
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .context("HTTP request for build info failed")?;

        if !resp.status().is_success() {
            bail!("Buildkite API returned status {}", resp.status());
        }

        let body: serde_json::Value = resp.json()?;
        let jobs = body["jobs"]
            .as_array()
            .context("No jobs array in build response")?;

        let mut result = Vec::new();
        for job in jobs {
            let job_type = job["type"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();

            if job_type != "script" {
                continue;
            }

            let name = job["step_key"]
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| {
                    job["name"]
                        .as_str()
                        .map(|n| strip_emoji_markup(n))
                })
                .unwrap_or_else(|| "unknown".to_string());

            result.push(JobInfo {
                id: job["id"].as_str().unwrap_or("").to_string(),
                name,
                step_key: job["step_key"].as_str().map(|s| s.to_string()),
                state: job["state"].as_str().unwrap_or("unknown").to_string(),
                job_type,
                retried: job["retried"].as_bool().unwrap_or(false),
                retries_count: job["retries_count"]
                    .as_u64()
                    .unwrap_or(0) as u32,
                retry_source_job_id: job["retry_source"]["job_id"]
                    .as_str()
                    .map(|s| s.to_string()),
                started_at: job["started_at"].as_str().map(|s| s.to_string()),
                finished_at: job["finished_at"].as_str().map(|s| s.to_string()),
            });
        }

        Ok(result)
    }

    pub fn retry_job(&self, parsed: &ParsedUrl) -> Result<serde_json::Value> {
        let job_id = parsed
            .job_id
            .as_deref()
            .context("Job ID is required to retry a specific job")?;

        let url = format!(
            "https://api.buildkite.com/v2/organizations/{}/pipelines/{}/builds/{}/jobs/{}/retry",
            parsed.org, parsed.pipeline, parsed.build_number, job_id
        );

        let resp = self
            .client
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .context("HTTP request to retry job failed")?;

        if !resp.status().is_success() {
            bail!(
                "Buildkite API returned status {}: {}",
                resp.status(),
                resp.text().unwrap_or_default()
            );
        }

        resp.json().context("Failed to parse retry response")
    }

    pub fn list_artifacts(&self, parsed: &ParsedUrl) -> Result<Vec<ArtifactInfo>> {
        let job_id = parsed
            .job_id
            .as_deref()
            .context("Job ID is required to list artifacts (use a URL with #job-id)")?;
        let url = format!(
            "https://api.buildkite.com/v2/organizations/{}/pipelines/{}/builds/{}/jobs/{}/artifacts",
            parsed.org, parsed.pipeline, parsed.build_number, job_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .context("HTTP request for artifacts failed")?;

        if !resp.status().is_success() {
            bail!(
                "Buildkite API returned status {}: {}",
                resp.status(),
                resp.text().unwrap_or_default()
            );
        }

        let body: Vec<serde_json::Value> = resp.json()?;
        let mut result = Vec::new();
        for artifact in &body {
            result.push(ArtifactInfo {
                id: artifact["id"].as_str().unwrap_or("").to_string(),
                filename: artifact["filename"].as_str().unwrap_or("").to_string(),
                path: artifact["path"].as_str().unwrap_or("").to_string(),
                size: artifact["file_size"].as_u64().unwrap_or(0),
                download_url: artifact["download_url"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            });
        }

        Ok(result)
    }

    pub fn download_artifact(&self, artifact: &ArtifactInfo) -> Result<Vec<u8>> {
        let resp = self
            .client
            .get(&artifact.download_url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .context("HTTP request to download artifact failed")?;

        if !resp.status().is_success() {
            bail!(
                "Buildkite API returned status {}: {}",
                resp.status(),
                resp.text().unwrap_or_default()
            );
        }

        resp.bytes()
            .map(|b| b.to_vec())
            .context("Failed to read artifact bytes")
    }

    pub fn fetch_job_name(&self, parsed: &ParsedUrl) -> Result<String> {
        let job_id = parsed
            .job_id
            .as_deref()
            .context("Job ID is required to fetch job name")?;

        let jobs = self.fetch_build_jobs(parsed)?;
        for job in &jobs {
            if job.id == job_id {
                return Ok(job.name.clone());
            }
        }

        bail!("Job {} not found in build", job_id)
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ArtifactInfo {
    pub id: String,
    pub filename: String,
    pub path: String,
    pub size: u64,
    pub download_url: String,
}

fn strip_emoji_markup(s: &str) -> String {
    let re = Regex::new(r":[a-zA-Z0-9_+-]+:\s*").unwrap();
    re.replace_all(s, "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_url_with_job_id() {
        let url = "https://buildkite.com/figma/figma/builds/5950766#019ca8a8-6e21-4548-9b5c-e8656a82feed";
        let parsed = parse_url(url).unwrap();
        assert_eq!(parsed.org, "figma");
        assert_eq!(parsed.pipeline, "figma");
        assert_eq!(parsed.build_number, "5950766");
        assert_eq!(
            parsed.job_id.as_deref(),
            Some("019ca8a8-6e21-4548-9b5c-e8656a82feed")
        );
    }

    #[test]
    fn test_parse_url_without_job_id() {
        let url = "https://buildkite.com/figma/ci/builds/287221";
        let parsed = parse_url(url).unwrap();
        assert_eq!(parsed.org, "figma");
        assert_eq!(parsed.pipeline, "ci");
        assert_eq!(parsed.build_number, "287221");
        assert_eq!(parsed.job_id, None);
    }

    #[test]
    fn test_strip_emoji_markup() {
        assert_eq!(
            strip_emoji_markup(":rust: multiplayer-rust-tests"),
            "multiplayer-rust-tests"
        );
        assert_eq!(strip_emoji_markup("plain-name"), "plain-name");
    }
}
