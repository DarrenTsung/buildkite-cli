mod buildkite;
mod github;
mod jobs;
mod log_parser;
mod output;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
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
}

#[derive(Subcommand)]
enum BuildsCommand {
    /// List all jobs in a build with pass/fail status
    ListJobs {
        /// Buildkite build URL (e.g. https://buildkite.com/figma/ci/builds/287221)
        url: String,
    },
}

#[derive(Subcommand)]
enum PrCommand {
    /// Show Buildkite check status for the current branch's PR
    Checks,
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
            BuildsCommand::ListJobs { url } => cmd_list_jobs(&url),
        },
        Commands::Pr { command } => match command {
            PrCommand::Checks => cmd_pr_checks(),
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
    }
}

fn cmd_pr_checks() -> Result<()> {
    let info = github::fetch_pr_checks().context("Failed to fetch PR checks")?;
    output::print_pr_checks(&info);
    Ok(())
}

fn cmd_list_jobs(url: &str) -> Result<()> {
    let parsed = buildkite::parse_url(url).context("Failed to parse Buildkite URL")?;
    let token =
        std::env::var("BUILDKITE_TOKEN").context("BUILDKITE_TOKEN environment variable not set")?;
    let client = buildkite::Client::new(&token);

    let jobs = client
        .fetch_build_jobs(&parsed)
        .context("Failed to fetch build jobs")?;

    output::print_build_jobs(&parsed.build_number, &parsed.pipeline, &jobs);
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

    // Parse with job-specific parser
    let parser = jobs::classify(&job_name);
    let result = parser.parse(&clean_lines);

    // Generate output
    output::write_results(&output_dir, &prefix, &build_number, &job_id, &job_name, &result)?;

    Ok(())
}

fn extract_build_from_filename(filename: &str) -> Option<String> {
    let re = regex::Regex::new(r"build_(\d+)").ok()?;
    re.captures(filename)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}
