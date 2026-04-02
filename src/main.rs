mod buildkite;
mod github;
mod jobs;
mod log_parser;
mod output;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use github::BkStepSummary;
use jobs::JobParser;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "bk", about = "Buildkite CLI for inspecting builds and job logs")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build-level operations
    Builds {
        #[command(subcommand)]
        command: BuildsCommand,
    },
    /// Job-level operations
    Jobs {
        #[command(subcommand)]
        command: JobsCommand,
    },
    /// GitHub PR operations
    Pr {
        #[command(subcommand)]
        command: PrCommand,
    },
    /// Retry a failed job
    Retry {
        /// Buildkite job URL (must include a #job-id fragment)
        url: String,
    },
}

#[derive(Subcommand)]
enum BuildsCommand {
    /// List all jobs in a build with pass/fail status
    ListJobs {
        /// Buildkite build URL (e.g. https://buildkite.com/figma/ci/builds/287221)
        url: String,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum PrCommand {
    /// Show Buildkite check status for the current branch's PR
    Checks {
        /// Branch name to check (defaults to current branch)
        #[arg(long)]
        branch: Option<String>,
    },
}

#[derive(Subcommand)]
enum JobsCommand {
    /// Download and parse job logs
    DownloadLogs {
        /// Buildkite job URL (e.g. https://buildkite.com/figma/figma/builds/5950766#019ca8a8-...)
        url: Option<String>,

        /// Read from a local log file instead of fetching from Buildkite API
        #[arg(long)]
        file: Option<PathBuf>,

        /// Job name to use when reading from a local file (e.g. "multiplayer-rust-tests")
        #[arg(long)]
        job_name: Option<String>,

        /// Dump cleaned logs only, skip structured parsing
        #[arg(long)]
        raw: bool,

        /// Output directory for generated files
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Builds { command } => match command {
            BuildsCommand::ListJobs { url, json } => cmd_list_jobs(&url, json),
        },
        Commands::Pr { command } => match command {
            PrCommand::Checks { branch } => cmd_pr_checks(branch),
        },
        Commands::Jobs { command } => match command {
            JobsCommand::DownloadLogs {
                url,
                file,
                job_name,
                raw,
                output_dir,
            } => cmd_download_logs(url, file, job_name, raw, output_dir),
        },
        Commands::Retry { url } => cmd_retry(&url),
    }
}

fn cmd_pr_checks(branch: Option<String>) -> Result<()> {
    let mut info =
        github::fetch_pr_checks(branch.as_deref()).context("Failed to fetch PR checks")?;

    // Expand each GitHub status check into its individual Buildkite jobs.
    if let Ok(token) = std::env::var("BUILDKITE_TOKEN") {
        let client = buildkite::Client::new(&token);
        expand_checks_to_jobs(&client, &mut info);
    }

    output::print_pr_checks(&info);
    Ok(())
}

/// Fetch Buildkite build jobs for every check and attach them as bk_steps.
/// This gives a full view of all running/passed/failed jobs, not just the
/// GitHub-level status.
fn expand_checks_to_jobs(client: &buildkite::Client, info: &mut github::PrInfo) {
    // Deduplicate builds: multiple checks may point to the same build URL.
    let mut builds: HashMap<String, Vec<buildkite::JobInfo>> = HashMap::new();

    for check in &info.checks {
        let build_url = check.link.split('#').next().unwrap_or(&check.link);
        if builds.contains_key(build_url) {
            continue;
        }
        if let Ok(parsed) = buildkite::parse_url(&check.link) {
            if let Ok(jobs) = client.fetch_build_jobs(&parsed) {
                builds.insert(build_url.to_string(), jobs);
            }
        }
    }

    if builds.is_empty() {
        return;
    }

    // Attach Buildkite job summaries to every check so effective state is
    // always computed from actual jobs, not GitHub's (sometimes premature)
    // bucket status. Display deduplication happens in the output layer.
    let mut step_cache: HashMap<String, Vec<BkStepSummary>> = HashMap::new();

    for check in &mut info.checks {
        let build_url = check
            .link
            .split('#')
            .next()
            .unwrap_or(&check.link)
            .to_string();
        let jobs = match builds.get(&build_url) {
            Some(j) => j,
            None => continue,
        };

        let steps = step_cache
            .entry(build_url)
            .or_insert_with(|| summarize_all_steps(jobs));
        check.bk_steps = steps.clone();
    }
}

/// Return step summaries for ALL jobs in a build.
fn summarize_all_steps(jobs: &[buildkite::JobInfo]) -> Vec<BkStepSummary> {
    let mut summaries = Vec::new();

    for job in jobs {
        // "broken" means a dependency failed, not this step itself.
        if job.state == "broken" {
            continue;
        }

        let prior_job_ids = match job.retry_source_job_id {
            Some(ref id) => vec![id.clone()],
            None => Vec::new(),
        };

        let duration_secs = compute_duration(&job.started_at, &job.finished_at);

        summaries.push(BkStepSummary {
            name: job.name.clone(),
            current_state: job.state.clone(),
            failed_attempts: job.retries_count,
            prior_job_ids,
            duration_secs,
            job_id: job.id.clone(),
        });
    }

    summaries
}

/// Compute duration in seconds from started_at to finished_at (or now).
fn compute_duration(started_at: &Option<String>, finished_at: &Option<String>) -> Option<u64> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let start = parse_iso8601(started_at.as_deref()?)?;
    let end = match finished_at {
        Some(f) => parse_iso8601(f)?,
        None => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs(),
    };
    Some(end.saturating_sub(start))
}

/// Minimal ISO 8601 parser for Buildkite timestamps (e.g. "2026-03-31T18:46:03.860Z").
fn parse_iso8601(s: &str) -> Option<u64> {
    // Strip fractional seconds and trailing Z.
    let s = s.trim_end_matches('Z');
    let (date_part, time_part) = s.split_once('T')?;
    let mut date_iter = date_part.split('-');
    let year: i64 = date_iter.next()?.parse().ok()?;
    let month: i64 = date_iter.next()?.parse().ok()?;
    let day: i64 = date_iter.next()?.parse().ok()?;

    let time_part = time_part.split('.').next()?; // drop fractional seconds
    let mut time_iter = time_part.split(':');
    let hour: i64 = time_iter.next()?.parse().ok()?;
    let min: i64 = time_iter.next()?.parse().ok()?;
    let sec: i64 = time_iter.next()?.parse().ok()?;

    // Days from year (simplified, good enough for 2000-2099).
    let mut days: i64 = 0;
    for y in 1970..year {
        days += if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
    }
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    for m in 0..(month - 1) as usize {
        days += month_days[m] as i64;
        if m == 1 && is_leap {
            days += 1;
        }
    }
    days += day - 1;

    Some((days * 86400 + hour * 3600 + min * 60 + sec) as u64)
}


fn cmd_list_jobs(url: &str, json: bool) -> Result<()> {
    let parsed = buildkite::parse_url(url).context("Failed to parse Buildkite URL")?;
    let token =
        std::env::var("BUILDKITE_TOKEN").context("BUILDKITE_TOKEN environment variable not set")?;
    let client = buildkite::Client::new(&token);

    let jobs = client
        .fetch_build_jobs(&parsed)
        .context("Failed to fetch build jobs")?;

    if json {
        output::print_build_jobs_json(&parsed, &jobs)?;
    } else {
        let base_url = format!(
            "https://buildkite.com/{}/{}/builds/{}",
            parsed.org, parsed.pipeline, parsed.build_number
        );
        output::print_build_jobs(&parsed.build_number, &parsed.pipeline, &base_url, &jobs);
    }
    Ok(())
}

fn cmd_retry(url: &str) -> Result<()> {
    let parsed = buildkite::parse_url(url).context("Failed to parse Buildkite URL")?;
    if parsed.job_id.is_none() {
        anyhow::bail!(
            "URL must include a #job-id fragment to retry a specific job.\n\
             Use: bk retry \"{}#<job-id>\"\n\
             To find job IDs, run: bk builds list-jobs \"{}\"",
            url,
            url
        );
    }
    let token =
        std::env::var("BUILDKITE_TOKEN").context("BUILDKITE_TOKEN environment variable not set")?;
    let client = buildkite::Client::new(&token);

    let resp = client.retry_job(&parsed).context("Failed to retry job")?;
    let new_id = resp["id"].as_str().unwrap_or("unknown");
    let state = resp["state"].as_str().unwrap_or("unknown");
    eprintln!(
        "Retried job in {}/{} build #{}: new job {} ({})",
        parsed.org, parsed.pipeline, parsed.build_number, new_id, state
    );

    Ok(())
}

fn cmd_download_logs(
    url: Option<String>,
    file: Option<PathBuf>,
    job_name_arg: Option<String>,
    raw: bool,
    output_dir: PathBuf,
) -> Result<()> {
    let (raw_log, build_number, job_id, job_name) = if let Some(file_path) = &file {
        let raw = std::fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read log file: {}", file_path.display()))?;

        let filename = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("local");
        let build_number =
            extract_build_from_filename(filename).unwrap_or_else(|| "local".into());
        let job_id = "local".to_string();
        let job_name = job_name_arg
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        (raw, build_number, job_id, job_name)
    } else if let Some(url) = &url {
        let parsed = buildkite::parse_url(url).context("Failed to parse Buildkite URL")?;
        if parsed.job_id.is_none() {
            anyhow::bail!(
                "URL is missing a job ID fragment (e.g. #019ca8a8-...).\n\
                 To download logs, use a URL like: {}#<job-id>\n\
                 To list all jobs in a build, use: bk builds list-jobs \"{}\"",
                url,
                url
            );
        }
        let token = std::env::var("BUILDKITE_TOKEN")
            .context("BUILDKITE_TOKEN environment variable not set")?;
        let client = buildkite::Client::new(&token);

        let job_name = client
            .fetch_job_name(&parsed)
            .unwrap_or_else(|_| "unknown".to_string());
        let raw = client
            .download_log(&parsed)
            .context("Failed to download log")?;

        let job_id = parsed.job_id.clone().unwrap_or_else(|| "unknown".into());
        (raw, parsed.build_number.clone(), job_id, job_name)
    } else {
        anyhow::bail!("Either a job URL or --file must be provided");
    };

    std::fs::create_dir_all(&output_dir)
        .with_context(|| format!("Failed to create output dir: {}", output_dir.display()))?;

    // Save raw log
    let prefix = format!("{}_{}", build_number, job_id);
    let raw_path = output_dir.join(format!("{}_raw.log", prefix));
    std::fs::write(&raw_path, &raw_log)?;
    eprintln!("Raw log saved to {}", raw_path.display());

    // Clean the log
    let clean_lines = log_parser::clean_log(&raw_log);

    // Save cleaned log
    let clean_path = output_dir.join(format!("{}_clean.log", prefix));
    let clean_text: String = clean_lines.iter().map(|l| format!("{}\n", l.text)).collect();
    std::fs::write(&clean_path, &clean_text)?;
    eprintln!("Cleaned log saved to {}", clean_path.display());

    if raw {
        print!("{}", clean_text);
        return Ok(());
    }

    // Parse with job-specific parser, falling back to generic error
    // extraction when the specialized parser finds nothing (e.g. a Go
    // compilation error in a lint or test job).
    let parser = jobs::classify(&job_name, &raw_log);
    let result = parser.parse(&clean_lines);
    let result = if result.is_empty() {
        let fallback = jobs::script_error::ScriptErrorParser;
        fallback.parse(&clean_lines)
    } else {
        result
    };

    // Scan for error-like lines the parser may have missed. This catches
    // bugs where a parser "succeeds" on some output but silently ignores
    // failures from other targets or languages in the same log.
    let uncaptured = jobs::find_uncaptured_errors(&clean_lines, &result);

    // Generate output
    output::write_results(&output_dir, &prefix, &build_number, &job_id, &job_name, &result, &uncaptured)?;

    Ok(())
}

fn extract_build_from_filename(filename: &str) -> Option<String> {
    let re = regex::Regex::new(r"build_(\d+)").ok()?;
    re.captures(filename)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}
