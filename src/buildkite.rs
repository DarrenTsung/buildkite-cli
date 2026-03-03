use anyhow::{Context, Result, bail};
use regex::Regex;

#[derive(Debug, Clone)]
pub struct ParsedUrl {
    pub org: String,
    pub pipeline: String,
    pub build_number: String,
    pub job_id: String,
}

pub fn parse_url(url: &str) -> Result<ParsedUrl> {
    // https://buildkite.com/figma/figma/builds/5950766#019ca8a8-6e21-4548-9b5c-e8656a82feed
    let re = Regex::new(
        r"https?://buildkite\.com/([^/]+)/([^/]+)/builds/(\d+)#([0-9a-f-]+)",
    )?;

    let caps = re.captures(url).context("URL does not match expected Buildkite format: https://buildkite.com/<org>/<pipeline>/builds/<number>#<job-id>")?;

    Ok(ParsedUrl {
        org: caps[1].to_string(),
        pipeline: caps[2].to_string(),
        build_number: caps[3].to_string(),
        job_id: caps[4].to_string(),
    })
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
        let url = format!(
            "https://api.buildkite.com/v2/organizations/{}/pipelines/{}/builds/{}/jobs/{}/log",
            parsed.org, parsed.pipeline, parsed.build_number, parsed.job_id
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

    pub fn fetch_job_name(&self, parsed: &ParsedUrl) -> Result<String> {
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

        for job in jobs {
            if job["id"].as_str() == Some(&parsed.job_id) {
                if let Some(name) = job["step_key"].as_str() {
                    return Ok(name.to_string()) as Result<String>;
                }
                if let Some(name) = job["name"].as_str() {
                    // Strip emoji markup like ":rust: multiplayer-rust-tests"
                    let cleaned = strip_emoji_markup(name);
                    return Ok(cleaned);
                }
            }
        }

        bail!("Job {} not found in build", parsed.job_id)
    }
}

fn strip_emoji_markup(s: &str) -> String {
    let re = Regex::new(r":[a-zA-Z0-9_+-]+:\s*").unwrap();
    re.replace_all(s, "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_url() {
        let url = "https://buildkite.com/figma/figma/builds/5950766#019ca8a8-6e21-4548-9b5c-e8656a82feed";
        let parsed = parse_url(url).unwrap();
        assert_eq!(parsed.org, "figma");
        assert_eq!(parsed.pipeline, "figma");
        assert_eq!(parsed.build_number, "5950766");
        assert_eq!(parsed.job_id, "019ca8a8-6e21-4548-9b5c-e8656a82feed");
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
