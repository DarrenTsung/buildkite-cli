mod buildkite;
mod jobs;
mod log_parser;
mod output;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "bk", about = "Parse Buildkite job logs and produce structured output")]
struct Cli {
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let (raw_log, build_number, job_id, job_name) = if let Some(file_path) = &cli.file {
        let raw = std::fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read log file: {}", file_path.display()))?;

        // Try to extract build number and job name from the filename
        let filename = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("local");
        let build_number = extract_build_from_filename(filename).unwrap_or_else(|| "local".into());
        let job_id = "local".to_string();
        let job_name = cli
            .job_name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        (raw, build_number, job_id, job_name)
    } else if let Some(url) = &cli.url {
        let parsed = buildkite::parse_url(url).context("Failed to parse Buildkite URL")?;
        let token = std::env::var("BUILDKITE_TOKEN")
            .context("BUILDKITE_TOKEN environment variable not set")?;
        let client = buildkite::Client::new(&token);

        let job_name = client
            .fetch_job_name(&parsed)
            .unwrap_or_else(|_| "unknown".to_string());
        let raw = client
            .download_log(&parsed)
            .context("Failed to download log")?;

        (raw, parsed.build_number.clone(), parsed.job_id.clone(), job_name)
    } else {
        anyhow::bail!("Either a Buildkite URL or --file must be provided");
    };

    std::fs::create_dir_all(&cli.output_dir)
        .with_context(|| format!("Failed to create output dir: {}", cli.output_dir.display()))?;

    // Save raw log
    let prefix = format!("{}_{}", build_number, job_id);
    let raw_path = cli.output_dir.join(format!("{}_raw.log", prefix));
    std::fs::write(&raw_path, &raw_log)?;
    eprintln!("Raw log saved to {}", raw_path.display());

    // Clean the log
    let clean_lines = log_parser::clean_log(&raw_log);

    // Save cleaned log
    let clean_path = cli.output_dir.join(format!("{}_clean.log", prefix));
    let clean_text: String = clean_lines.iter().map(|l| format!("{}\n", l.text)).collect();
    std::fs::write(&clean_path, &clean_text)?;
    eprintln!("Cleaned log saved to {}", clean_path.display());

    if cli.raw {
        print!("{}", clean_text);
        return Ok(());
    }

    // Parse with job-specific parser
    let parser = jobs::classify(&job_name);
    let result = parser.parse(&clean_lines);

    // Generate output
    output::write_results(
        &cli.output_dir,
        &prefix,
        &build_number,
        &job_id,
        &job_name,
        &result,
    )?;

    Ok(())
}

fn extract_build_from_filename(filename: &str) -> Option<String> {
    // Match pattern like "figma_build_5950766_multiplayer-rust-tests"
    let re = regex::Regex::new(r"build_(\d+)").ok()?;
    re.captures(filename)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}
