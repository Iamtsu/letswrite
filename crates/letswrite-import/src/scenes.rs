//! Derive `scenes` rows from chapter bodies by splitting on the
//! `## Beat N: Title` heading convention used in our dogfood novel.
//!
//! A "beat" heading is any line that looks like `## Beat <number>: <title>`
//! (case-insensitive, with arbitrary whitespace). Each beat becomes one
//! scene; offsets are byte positions into the document body (with
//! frontmatter already stripped by `Document`). The first beat may have
//! body text before it — that pre-beat text isn't a scene, it's chapter
//! preamble and is dropped here.

use std::path::PathBuf;

use rusqlite::params;

use letswrite_core::{Document, DocumentKind, Result};

#[allow(clippy::redundant_pub_crate)]
pub(crate) struct Outcome {
    pub scenes: usize,
}

#[allow(clippy::redundant_pub_crate)]
pub(crate) fn import(
    tx: &rusqlite::Transaction<'_>,
    project_id: i64,
    docs: &[(DocumentKind, PathBuf, Document)],
) -> Result<Outcome> {
    let mut scenes = 0;

    for (kind, _, doc) in docs {
        if *kind != DocumentKind::Chapter {
            continue;
        }
        let document_id = super::document_id(tx, project_id, &doc.rel_path)?;
        let beats = parse_beats(&doc.body);
        for (i, beat) in beats.iter().enumerate() {
            // Gap-friendly ordering so future reorders don't have to rewrite
            // every row.
            #[allow(clippy::cast_precision_loss)]
            let order_index: f64 = ((i + 1) * 1000) as f64;
            // SQLite offsets are stored as INTEGER. usize → i64 is safe for
            // any realistic document size (i64 max is ~9 EB).
            #[allow(clippy::cast_possible_wrap)]
            let start = beat.start_offset as i64;
            #[allow(clippy::cast_possible_wrap)]
            let end = beat.end_offset as i64;
            tx.execute(
                "INSERT INTO scenes
                    (document_id, order_index, synopsis, status, start_offset, end_offset)
                 VALUES (?1, ?2, ?3, 'draft', ?4, ?5)",
                params![document_id, order_index, beat.title, start, end],
            )?;
            scenes += 1;
        }
    }
    Ok(Outcome { scenes })
}

#[derive(Debug, PartialEq, Eq)]
struct Beat {
    title: String,
    start_offset: usize,
    end_offset: usize,
}

/// Find every `## Beat N: Title` heading and slice the body into beats.
/// Body before the first beat (chapter preamble) is discarded — see module
/// docstring.
fn parse_beats(body: &str) -> Vec<Beat> {
    let mut hits: Vec<(usize, &str)> = Vec::new();
    for (line_start, line) in line_starts(body) {
        if let Some(title) = parse_beat_heading(line) {
            hits.push((line_start, title));
        }
    }
    let mut out = Vec::with_capacity(hits.len());
    for i in 0..hits.len() {
        let (start, title) = hits[i];
        let end = if i + 1 < hits.len() {
            hits[i + 1].0
        } else {
            body.len()
        };
        out.push(Beat {
            title: title.to_owned(),
            start_offset: start,
            end_offset: end.max(start + 1), // CHECK constraint requires end > start
        });
    }
    out
}

/// Iterate over `(byte_offset, line_without_terminator)` pairs.
fn line_starts(body: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut start = 0;
    let bytes = body.as_bytes();
    let len = bytes.len();
    std::iter::from_fn(move || {
        if start > len {
            return None;
        }
        let line_start = start;
        let mut i = start;
        while i < len && bytes[i] != b'\n' {
            i += 1;
        }
        let line_end = i;
        // Strip a trailing `\r` for CRLF safety.
        let trimmed_end = if line_end > line_start && bytes[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };
        let line = &body[line_start..trimmed_end];
        start = if i < len { i + 1 } else { len + 1 };
        Some((line_start, line))
    })
}

/// Match `## Beat <num>: <title>` (case-insensitive on the literal "Beat").
/// Returns the trimmed title on a match.
fn parse_beat_heading(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("## ")?;
    let rest = strip_prefix_case_insensitive(rest, "beat")?;
    let rest = rest.trim_start();
    // Number is optional in case someone writes `## Beat: ...` — we still
    // count it as a beat heading.
    let after_num = rest.find(':').and_then(|colon| {
        let (num, after) = rest.split_at(colon);
        // num should be empty or digits/whitespace.
        if num.chars().all(|c| c.is_ascii_digit() || c.is_whitespace()) {
            Some(after.trim_start_matches(':').trim())
        } else {
            None
        }
    })?;
    if after_num.is_empty() {
        None
    } else {
        Some(after_num)
    }
}

fn strip_prefix_case_insensitive<'a>(s: &'a str, needle: &str) -> Option<&'a str> {
    if s.len() < needle.len() {
        return None;
    }
    let prefix = &s[..needle.len()];
    if prefix.eq_ignore_ascii_case(needle) {
        Some(&s[needle.len()..])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_classic_beat_headings() {
        let body = "preamble\n\n## Beat 1: The Fog\n\nbody one\n\n## Beat 2: The Summons\n\nbody two\n";
        let beats = parse_beats(body);
        assert_eq!(beats.len(), 2);
        assert_eq!(beats[0].title, "The Fog");
        assert_eq!(beats[1].title, "The Summons");
        // The second beat starts where its heading line starts.
        let beat2_start = body.find("## Beat 2").unwrap();
        assert_eq!(beats[1].start_offset, beat2_start);
        // The first beat ends where the second begins.
        assert_eq!(beats[0].end_offset, beat2_start);
    }

    #[test]
    fn case_insensitive_keyword() {
        let body = "## beat 3: lowercase\n\nx\n";
        let beats = parse_beats(body);
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].title, "lowercase");
    }

    #[test]
    fn ignores_non_beat_h2() {
        let body = "## Chapter Outline\n\ntext\n\n## Beat 1: Real\n\nmore\n";
        let beats = parse_beats(body);
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].title, "Real");
    }

    #[test]
    fn returns_empty_when_no_beats_present() {
        let body = "# Chapter Title\n\nplain prose with no beats\n";
        assert!(parse_beats(body).is_empty());
    }

    #[test]
    fn handles_crlf_line_endings() {
        let body = "## Beat 1: A\r\n\r\nbody\r\n";
        let beats = parse_beats(body);
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].title, "A");
    }

    #[test]
    fn beat_without_number_still_counts() {
        let body = "## Beat: Untitled\n\nbody\n";
        let beats = parse_beats(body);
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].title, "Untitled");
    }

    #[test]
    fn empty_title_rejected() {
        let body = "## Beat 1: \n\nbody\n";
        assert!(parse_beats(body).is_empty());
    }
}
