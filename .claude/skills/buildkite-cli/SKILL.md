---
name: buildkite-cli
description: Buildkite CLI for inspecting builds and parsing job logs. Use when the user shares a Buildkite build URL, wants to list jobs in a build, analyze CI test failures, asks about failing tests in a Buildkite job, or wants to check PR CI status from the current branch.
---

# Buildkite CLI (`bk`)

The `bk` CLI inspects Buildkite builds and parses job logs. It can list all jobs in a build with pass/fail status, or download and parse individual job logs into structured output.

## Authentication

Set the Buildkite API token (requires `read_builds` and `write_builds` scopes):

```bash
export BUILDKITE_TOKEN="your-token"
```

Tokens can be created at: https://buildkite.com/user/api-access-tokens

## Commands

### `bk pr checks [--branch <BRANCH>]`

Shows Buildkite check status for a PR. Defaults to the current branch's PR; use `--branch` to check a different branch. Requires `gh` CLI to be authenticated.

When `BUILDKITE_TOKEN` is set, pending checks are enriched with actual Buildkite job states. This detects jobs that are failing behind a "pending" GitHub status (e.g. jobs in a retry loop that GitHub hasn't marked as failed yet).

```bash
bk pr checks
bk pr checks --branch darren/my-feature
```

Example output:

```
PR #1234: my-feature-branch

  ✓ lint-check
  ✓ type-check
  ✗ multiplayer-rust-tests (failed)
    https://buildkite.com/figma/figma/builds/5950766#019ca8a8-6e21-4548-9b5c-e8656a82feed
  ✓ multiplayer-typescript-tests
  ⚠ agentplat-itest (pending — 1 job failing)
    https://buildkite.com/figma/agentplat-itest/builds/54321
      ✗ agentplat-itest: failed (2 prior attempts failed)
  - deploy-staging (pending)

3 passed, 1 failed, 1 failing (still pending on GitHub), 1 pending
```

Failed and pending checks show their Buildkite URL, which you can pass directly to `bk jobs download-logs` or `bk builds list-jobs`.

### `bk builds list-jobs <BUILD_URL> [--json]`

Lists all jobs in a build with pass/fail status. Each job shows its full Buildkite URL (dimmed) on the next line for easy copy-paste into `bk jobs download-logs`.

```bash
bk builds list-jobs "https://buildkite.com/figma/ci/builds/287221"
```

Example output:

```
Build 287221 (ci)

  ✓ lint-check
    https://buildkite.com/figma/ci/builds/287221#aaa-bbb
  ✓ type-check
    https://buildkite.com/figma/ci/builds/287221#ccc-ddd
  ✗ multiplayer-rust-tests (failed)
    https://buildkite.com/figma/ci/builds/287221#eee-fff
  ✓ multiplayer-typescript-tests
    https://buildkite.com/figma/ci/builds/287221#ggg-hhh
  - deploy-staging (waiting)
    https://buildkite.com/figma/ci/builds/287221#iii-jjj
```

Use `--json` to output structured JSON with `name`, `id`, `state`, and `url` fields:

```bash
bk builds list-jobs "https://buildkite.com/figma/ci/builds/287221" --json
```

### `bk retry <JOB_URL>`

Retries a specific failed job. The URL must include a `#job-id` fragment.

```bash
bk retry "https://buildkite.com/figma/figma/builds/5950766#019ca8a8-6e21-4548-9b5c-e8656a82feed"
```

### `bk jobs download-logs <JOB_URL>`

Downloads and parses a job's log. The URL must include a `#job-id` fragment.

```bash
bk jobs download-logs "https://buildkite.com/figma/figma/builds/5950766#019ca8a8-6e21-4548-9b5c-e8656a82feed"
```

### `bk jobs download-logs --file <PATH>`

Parses a local log file instead of fetching from the API.

```bash
bk jobs download-logs --file path/to/log.log --job-name multiplayer-rust-tests
```

### Flags for `jobs download-logs`

| Flag             | Required | Description                                                        |
|------------------|----------|--------------------------------------------------------------------|
| `<URL>`          | No*      | Buildkite job URL with `#job-id` fragment                          |
| `--file <path>`  | No*      | Local log file to parse instead of fetching from API               |
| `--job-name`     | No       | Job name hint when using `--file` (e.g. `multiplayer-rust-tests`)  |
| `--raw`          | No       | Output cleaned log only, skip structured parsing                   |
| `--output-dir`   | No       | Output directory (default: `.`). **Always use the default (current directory) — do not use `/tmp/`.** |

*Either a job URL or `--file` must be provided.

## Output files

All files are written to `--output-dir` (default: current directory):

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

### `agentplat-test` and Go test jobs (gotest)

Parses Go test output run via Bazel. Matches job names containing `agentplat` or `go-test`. Extracts:
- Per-package results grouped by Bazel target
- Individual test pass/fail/skip with durations
- Failure output (error messages between `=== RUN` and `--- FAIL`)
- Bazel summary (executed/total/passed/failed)
- Distinguishes executed vs cached targets (using Bazel progress line durations)
- Supports both `Test output for` (batch) and `@@//target:binary |` (streaming) Bazel formats
- **Non-Go target failures** (e.g. TS client itests): detects Bazel targets that fail without Go test output patterns. Captures raw error output and Bazel `FAILED`/`TIMEOUT` lines so these failures aren't silently hidden behind passing Go tests.

### Generic script errors (fallback)

For any unrecognized job type, extracts error lines (`ERROR:`, TypeScript errors, Go compiler errors, `❌` markers, Bazel `FAILED` targets), the failed command, and exit code from the log. Go compiler errors (`undefined:`, `too many arguments`, etc.) include have/want type context lines.

When a specialized parser (gotest, golint, etc.) returns empty results (e.g. a compilation error prevents tests or linting from running), the CLI automatically falls back to this generic parser to surface the actual errors.

### Lint jobs (golint)

Parses golangci-lint output. Matches job names containing `lint`. Extracts:
- Lint issues with file, line, column, message, and linter name

## Examples

```bash
# Show Buildkite checks for the current branch's PR
bk pr checks

# Show Buildkite checks for a specific branch's PR
bk pr checks --branch darren/my-feature

# List all jobs in a build
bk builds list-jobs "https://buildkite.com/figma/ci/builds/287221"

# Download and parse a specific job's log
bk jobs download-logs "https://buildkite.com/figma/figma/builds/5950766#019ca8a8-6e21-4548-9b5c-e8656a82feed"

# Parse a previously downloaded log
bk jobs download-logs --file figma_build_5950766_multiplayer-rust-tests.log --job-name multiplayer-rust-tests

# Retry a specific failed job
bk retry "https://buildkite.com/figma/figma/builds/5950766#019ca8a8-6e21-4548-9b5c-e8656a82feed"

# Just get the cleaned log for manual inspection
bk jobs download-logs --file build.log --raw | less

# Extract failing test names from JSON
cat *_results.json | jq '.runs[].failing_tests[].name'
```

## Building

The CLI is a Rust project at `/Users/dtsung/Documents/buildkite-cli`:

```bash
cd /Users/dtsung/Documents/buildkite-cli
cargo build --release
```

The binary is at `target/release/bk`.
