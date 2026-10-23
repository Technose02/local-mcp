//! Pure text utilities for in-document search: regex matching with context
//! windows and markdown heading outlines. Offsets are in **characters** so
//! they can be fed back into `fetch_url`'s `start_char` paging.

use regex::RegexBuilder;

/// One matched window inside a document.
#[derive(Debug, Clone)]
pub struct TextMatch {
    pub start_char: usize,
    pub end_char: usize,
    pub snippet: String,
}

/// A heading in a markdown document.
#[derive(Debug, Clone)]
pub struct OutlineEntry {
    pub level: u8,
    pub title: String,
    pub start_char: usize,
}

/// Safety cap on the number of regex matches collected before merging.
const RAW_MATCH_CAP: usize = 200;
/// Cap on headings returned by [`outline`].
const OUTLINE_CAP: usize = 120;

/// Case-insensitive `pattern` search returning merged context windows.
/// Returns `(matches, total_match_count)`; at most `max_matches` windows are
/// returned, but the count covers every match in the document.
pub fn find_windows(
    text: &str,
    pattern: &str,
    context_chars: usize,
    max_matches: usize,
) -> Result<(Vec<TextMatch>, usize), String> {
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .map_err(|error| format!("invalid pattern '{pattern}': {error}"))?;

    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut count = 0usize;
    let mut byte_cursor = 0usize;
    let mut char_cursor = 0usize;
    for found in regex.find_iter(text) {
        count += 1;
        if spans.len() < RAW_MATCH_CAP {
            char_cursor += text[byte_cursor..found.start()].chars().count();
            byte_cursor = found.end();
            let length = found.as_str().chars().count();
            spans.push((char_cursor, char_cursor + length));
            char_cursor += length;
        }
    }
    if count == 0 {
        return Ok((Vec::new(), 0));
    }

    let context = context_chars.max(40);
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for (start, end) in spans {
        let start = start.saturating_sub(context);
        let end = (end + context).min(chars.len());
        match windows.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => windows.push((start, end)),
        }
    }

    let matches = windows
        .into_iter()
        .take(max_matches)
        .map(|(start, end)| TextMatch {
            start_char: start,
            end_char: end,
            snippet: snippet(&chars, start, end),
        })
        .collect();
    Ok((matches, count))
}

fn snippet(chars: &[char], start: usize, end: usize) -> String {
    let body: String = chars[start..end].iter().collect();
    let body = body.replace("\n\n", "\n");
    let body = body.trim();
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < chars.len() { "…" } else { "" };
    format!("{prefix}{body}{suffix}")
}

/// Extract a markdown heading outline (`#`..`######`) with character offsets.
pub fn outline(text: &str) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();
    let mut position = 0usize;
    for line in text.lines() {
        if entries.len() >= OUTLINE_CAP {
            break;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('#') {
            let extra = rest.chars().take_while(|c| *c == '#').count();
            let level = 1 + extra;
            let tail = &rest[extra..];
            if (1..=6).contains(&level) && tail.starts_with(' ') {
                let title = tail.trim().trim_end_matches('#').trim();
                if !title.is_empty() {
                    entries.push(OutlineEntry {
                        level: level as u8,
                        title: title.to_string(),
                        start_char: position,
                    });
                }
            }
        }
        position += line.chars().count() + 1;
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_windows_merges_overlapping_context() {
        let text = "alpha slot-save-path beta slot-save-path gamma";
        let (matches, count) = find_windows(text, "slot-save-path", 50, 10).expect("valid");
        assert_eq!(count, 2);
        // Both matches share one merged context window.
        assert_eq!(matches.len(), 1);
        assert!(matches[0].snippet.contains("slot-save-path"));
    }

    #[test]
    fn find_windows_counts_all_matches_but_caps_windows() {
        let unit = format!("{} keep {}", "z".repeat(40), "z".repeat(40));
        let text = vec![unit; 30].join(" ");
        let (matches, count) = find_windows(&text, "keep", 5, 5).expect("valid");
        assert_eq!(count, 30);
        assert_eq!(matches.len(), 5);
    }

    #[test]
    fn find_windows_rejects_bad_pattern() {
        assert!(find_windows("text", "(unclosed", 10, 5).is_err());
    }

    #[test]
    fn find_windows_handles_multibyte_offsets() {
        let text = "frühes Treffer-spät später".to_string();
        let (matches, count) = find_windows(&text, "Treffer", 3, 5).expect("valid");
        assert_eq!(count, 1);
        assert_eq!(matches[0].start_char, 0);
        let body: String = text.chars().collect::<Vec<_>>()[7..14].iter().collect();
        assert_eq!(body, "Treffer");
    }

    #[test]
    fn outline_lists_headings_with_offsets() {
        let text = "# Title\n\ntext\n\n## API Endpoints\n\nmore\n\n### /slots\n";
        let entries = outline(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].title, "Title");
        assert_eq!(entries[1].title, "API Endpoints");
        assert_eq!(entries[2].level, 3);
        assert_eq!(&text.chars().collect::<String>()[entries[2].start_char..], "### /slots\n");
    }

    #[test]
    fn outline_ignores_non_headings() {
        let entries = outline("just text\n#not a heading\n  # real");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "real");
        assert!(outline("no headings").is_empty());
    }
}
