use regex::Regex;

#[derive(Debug, Clone)]
pub struct CleanLine {
    pub text: String,
    #[allow(dead_code)]
    pub timestamp_ms: Option<u64>,
}

pub fn clean_log(raw: &str) -> Vec<CleanLine> {
    // BK prefix is \x1b_bk;t=<timestamp>\x07 (ESC + _bk;t=<digits> + BEL)
    let bk_ts_re = Regex::new(r"\x1b_bk;t=(\d+)\x07").unwrap();
    // Also handle plain-text _bk;t= (for testing with sanitized input)
    let bk_ts_plain_re = Regex::new(r"^_bk;t=(\d+)").unwrap();
    let ansi_re = Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
    let cursor_re = Regex::new(r"\x1b\[1A\x1b\[K|\[1A\[K").unwrap();
    let bazel_progress_re = Regex::new(r"^\s*\[\d[\d,]* / \d[\d,]*\]").unwrap();
    let bazel_testing_re =
        Regex::new(r"^\s*(?:Testing |\.\.\./).*;\s+\d+s\s+linux-sandbox").unwrap();

    let mut lines = Vec::new();

    for raw_line in raw.lines() {
        // Strip trailing \r
        let raw_line = raw_line.trim_end_matches('\r');

        let mut timestamp_ms = None;
        let mut line = raw_line.to_string();

        // Extract timestamp from BK prefix(es) - take the last one if multiple
        for caps in bk_ts_re.captures_iter(raw_line) {
            timestamp_ms = caps[1].parse().ok();
        }

        // Strip all BK timestamp prefixes (ESC-based)
        line = bk_ts_re.replace_all(&line, "").to_string();

        // Also try plain-text format
        if timestamp_ms.is_none() {
            if let Some(caps) = bk_ts_plain_re.captures(&line) {
                timestamp_ms = caps[1].parse().ok();
                line = line[caps[0].len()..].to_string();
            }
        }

        // Strip cursor control sequences
        line = cursor_re.replace_all(&line, "").to_string();

        // Strip ANSI escape sequences
        line = ansi_re.replace_all(&line, "").to_string();

        // Strip BEL characters and other control chars (except newline/tab)
        line = line
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .collect();

        // Skip empty lines
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Filter out bazel progress lines
        if bazel_progress_re.is_match(trimmed) {
            continue;
        }

        // Filter out bazel "Testing ..." progress lines
        if bazel_testing_re.is_match(trimmed) {
            continue;
        }

        lines.push(CleanLine {
            text: line,
            timestamp_ms,
        });
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_bk_timestamp_esc() {
        let input = "\x1b_bk;t=1772356394904\x07        PASS [   0.068s] multiplayer_rollout_test_nextest dirty_document_checkpoints::test::query_ranges_with_multiple_ranges";
        let lines = clean_log(input);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].text.contains("PASS"));
        assert_eq!(lines[0].timestamp_ms, Some(1772356394904));
    }

    #[test]
    fn test_strip_ansi() {
        let input =
            "\x1b_bk;t=1772356155147\x07\x1b[90m# SSH_AUTH_SOCK added\x1b[0m";
        let lines = clean_log(input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "# SSH_AUTH_SOCK added");
    }

    #[test]
    fn test_filter_bazel_progress() {
        let input = "\x1b_bk;t=1772356599791\x07\x1b[32m[2,086 / 2,087]\x1b[0m 2 / 5 tests;\x1b[0m  1 action\x1b[0m";
        let lines = clean_log(input);
        assert!(lines.is_empty(), "Bazel progress line should be filtered out");
    }

    #[test]
    fn test_cursor_control_with_embedded_timestamp() {
        let input = "\x1b_bk;t=1772356395003\x07\r\x1b[1A\x1b[K\x1b_bk;t=1772356395003\x07        PASS [   0.096s] multiplayer_rollout_test_nextest dirty_document_checkpoints::test::query_ranges_with_single_range_and_dirty_documents";
        let lines = clean_log(input);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].text.contains("PASS"));
    }

    #[test]
    fn test_filter_testing_progress() {
        let input = "\x1b_bk;t=1772356599791\x07    Testing //.../multiplayer:multiplayer_test_nextest; 128s linux-sandbox";
        let lines = clean_log(input);
        assert!(lines.is_empty());
    }
}
