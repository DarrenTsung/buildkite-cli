---
name: bk-logs
description: Parse Buildkite job logs to extract test results, failures, and structured output. Use when the user shares a Buildkite build URL, wants to analyze CI test failures, or asks about failing tests in a Buildkite job.
---

# Buildkite Log Parser CLI

The `bk` CLI downloads and parses Buildkite job logs, stripping noise (BK timestamps, ANSI codes, bazel progress lines) and producing structured output for known job types (currently `multiplayer-rust-tests` nextest jobs).

## Authentication

Set the Buildkite API token (requires `read_builds` scope):

```bash
export BUILDKITE_TOKEN="your-token"
```

Tokens can be created at: https://buildkite.com/user/api-access-tokens

## Usage

### Parse a Buildkite job URL

```bash
bk "https://buildkite.com/figma/figma/builds/5950766#019ca8a8-6e21-4548-9b5c-e8656a82feed"
```

### Parse a local log file

```bash
bk --file path/to/log.log --job-name multiplayer-rust-tests
```

### Dump cleaned log only (no structured parsing)

```bash
bk --file path/to/log.log --raw
```

### Custom output directory

```bash
bk "https://buildkite.com/..." --output-dir /tmp/my-logs
```

## Flags reference

| Flag             | Required | Description                                                        |
|------------------|----------|--------------------------------------------------------------------|
| `<URL>`          | No*      | Buildkite job URL (org/pipeline/build/job extracted from URL)      |
| `--file <path>`  | No*      | Local log file to parse instead of fetching from API               |
| `--job-name`     | No       | Job name hint when using `--file` (e.g. `multiplayer-rust-tests`)  |
| `--raw`          | No       | Output cleaned log only, skip structured parsing                   |
| `--output-dir`   | No       | Output directory (default: `/tmp/bk-logs`)                         |

*Either a Buildkite URL or `--file` must be provided.

## Output files

All files are written to `--output-dir` (default `/tmp/bk-logs/`):

| File                        | Description                                          |
|-----------------------------|------------------------------------------------------|
| `{build}_{job}_raw.log`    | Original raw log                                     |
| `{build}_{job}_clean.log`  | Cleaned log (BK timestamps, ANSI, bazel noise removed) |
| `{build}_{job}_results.json` | Structured JSON with all test results              |
| `{build}_{job}_summary.txt`  | Human-readable summary with failures               |

## Supported job types

### `multiplayer-rust-tests` (nextest)

Parses nextest output across multiple bazel targets. For each nextest run, extracts:
- Per-test results (PASS, FAIL, TIMEOUT) with durations
- Failure details with captured stdout and stderr
- Run summaries (total, passed, failed, timed out, skipped)

### `multiplayer-typescript-tests` (mocha)

Parses mocha test output. Extracts:
- Summary counts (passing, pending, failing)
- Each failure with suite name, test name, error message, diff, and stack trace
- Suppressed known flakes from the CI test-runner

## Examples

```bash
# Analyze a failing CI build
bk "https://buildkite.com/figma/figma/builds/5950766#019ca8a8-6e21-4548-9b5c-e8656a82feed"

# Parse a previously downloaded log
bk --file figma_build_5950766_multiplayer-rust-tests.log --job-name multiplayer-rust-tests

# Just get the cleaned log for manual inspection
bk --file build.log --raw | less

# Extract failing test names from JSON
cat /tmp/bk-logs/*_results.json | jq '.runs[].failing_tests[].name'
```

## Building

The CLI is a Rust project at `/Users/dtsung/Documents/buildkite-cli`:

```bash
cd /Users/dtsung/Documents/buildkite-cli
cargo build --release
```

The binary is at `target/release/bk`.
