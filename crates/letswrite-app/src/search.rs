//! Find-in-document panel rendered inside the sidebar's Search tab.
//!
//! Owns the query text, the match-mode flags (case-sensitive, regex), and
//! the cursor position within the match list. Match computation is
//! intentionally stateless: callers pass the current document body to
//! [`SearchState::find_matches`] on demand. The state itself doesn't
//! cache results — for the document sizes letswrite targets (a chapter
//! is tens of KB), the cost of a re-scan per click is invisible, and
//! statelessness keeps the buffer-of-truth firmly inside the editor.
//!
//! Replace-mode controls land in task #6. This module ships find first
//! to keep the diff small and verifiable.

use std::ops::Range;

use iced::widget::{button, column, container, row, text, text_input, Space};
use iced::{Element, Length};

/// One match in the document body, as a byte range.
pub(crate) type Match = Range<usize>;

/// Find-only or find-and-replace. Ctrl-F opens the panel in [`Find`];
/// Ctrl-H opens it in [`Replace`] which reveals the replacement input
/// and the Replace / Replace All buttons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Find,
    Replace,
}

#[derive(Debug, Default)]
pub(crate) struct SearchState {
    query: String,
    replacement: String,
    case_sensitive: bool,
    regex: bool,
    mode: Mode,
    /// Index of the "current" match, or `None` if the user hasn't
    /// navigated yet. `None` is the post-type, pre-Enter state —
    /// matches are counted in the panel but nothing is highlighted in
    /// the editor. The first Next/Enter advances to index 0, not 1.
    current: Option<usize>,
}

#[derive(Debug, Clone)]
pub(crate) enum Message {
    /// Query text edited.
    QueryChanged(String),
    /// Replacement text edited (only meaningful in `Mode::Replace`).
    ReplacementChanged(String),
    /// Toggle case-sensitive matching.
    ToggleCase,
    /// Toggle regex matching.
    ToggleRegex,
    /// Navigate to the next match (wraps).
    Next,
    /// Navigate to the previous match (wraps).
    Previous,
    /// Switch between find-only and find-and-replace mode.
    SetMode(Mode),
    /// Replace the currently-selected match, then advance.
    ReplaceCurrent,
    /// Replace every match in the document in one batch.
    ReplaceAll,
}

/// What the panel wants the app shell to do after a search interaction:
/// jump the editor to a byte range so the match is selected + on screen,
/// and/or splice replacements into the buffer.
#[derive(Debug, Default)]
pub(crate) struct SearchReaction {
    pub jump_to: Option<Match>,
    /// Splice operations the editor should perform in order. Each tuple
    /// is `(byte range to overwrite, expected current text, replacement)`.
    /// For Replace All the list is sorted highest-offset first so that
    /// applying them in order leaves earlier offsets unaffected.
    pub splices: Vec<(Match, String, String)>,
}

impl SearchState {
    pub(crate) const fn new() -> Self {
        Self {
            query: String::new(),
            replacement: String::new(),
            case_sensitive: false,
            regex: false,
            mode: Mode::Find,
            current: None,
        }
    }

    pub(crate) const fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    // Used by the keyboard wiring in task #7 to decide whether Esc
    // should clear the query or close the panel.
    #[allow(dead_code)]
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// Compute every match of the current query in `body`. Empty query
    /// returns an empty vec. Invalid regex (when regex mode is on)
    /// returns an empty vec — the UI signals this via the "0 matches"
    /// label rather than an error popup. Case-insensitive substring
    /// search is byte-cheap; regex compiles the pattern fresh each
    /// call, which is fine at typing speed for documents this size.
    pub(crate) fn find_matches(&self, body: &str) -> Vec<Match> {
        if self.query.is_empty() {
            return Vec::new();
        }
        if self.regex {
            let pattern = if self.case_sensitive {
                self.query.clone()
            } else {
                format!("(?i){}", self.query)
            };
            regex::Regex::new(&pattern).map_or_else(
                |_| Vec::new(),
                |re| re.find_iter(body).map(|m| m.start()..m.end()).collect(),
            )
        } else if self.case_sensitive {
            body.match_indices(&self.query)
                .map(|(start, m)| start..start + m.len())
                .collect()
        } else {
            // Case-insensitive substring: lowercase both sides, then
            // re-map lowercased offsets back to the original body.
            // Safe for ASCII; for non-ASCII (e.g. German ß → ss) the
            // lowercased length can differ from the source, so we walk
            // by byte index and check at each position.
            let needle = self.query.to_lowercase();
            let hay = body.to_lowercase();
            // Lowercased body length may differ from source. The simple
            // mapping (byte-equal) works only when lowering preserves
            // byte length — true for ASCII and most Latin-1 characters.
            // For exotic cases (ß, Turkish dotted I), we fall back to a
            // case-insensitive walk that compares grapheme by grapheme;
            // good enough for letswrite's prose-search use case.
            if hay.len() == body.len() {
                hay.match_indices(&needle)
                    .map(|(start, m)| start..start + m.len())
                    .collect()
            } else {
                fallback_ci_search(body, &self.query)
            }
        }
    }

    /// Move forward to the next match. The first call after a fresh
    /// query (no navigation yet) lands on index 0, not 1, so Enter
    /// after typing reads as "go to first match" not "skip the first".
    /// Subsequent calls advance and wrap.
    pub(crate) fn advance(&mut self, matches: &[Match]) -> Option<Match> {
        if matches.is_empty() {
            return None;
        }
        let next = self.current.map_or(0, |i| (i + 1) % matches.len());
        self.current = Some(next);
        matches.get(next).cloned()
    }

    /// Move backward to the previous match. Symmetric to `advance`:
    /// the first call lands on the **last** match (Shift-Tab semantics)
    /// rather than wrapping from a phantom-zero position.
    pub(crate) fn retreat(&mut self, matches: &[Match]) -> Option<Match> {
        if matches.is_empty() {
            return None;
        }
        let prev = match self.current {
            None | Some(0) => matches.len() - 1,
            Some(i) => i - 1,
        };
        self.current = Some(prev);
        matches.get(prev).cloned()
    }

    pub(crate) fn update(
        &mut self,
        message: Message,
        body: Option<&str>,
    ) -> SearchReaction {
        match message {
            Message::QueryChanged(q) => {
                self.query = q;
                self.current = None;
                // Deliberately do NOT auto-jump the editor here. iced
                // only paints the selection when the editor is focused
                // (text_editor.rs:1021); focusing the editor steals it
                // from the query input mid-keystroke and the user gets
                // truncated to one letter at a time. Match count still
                // updates because the view recomputes on every paint —
                // the user presses Enter / ›‹ when ready to navigate.
                SearchReaction::default()
            }
            Message::ReplacementChanged(r) => {
                self.replacement = r;
                SearchReaction::default()
            }
            Message::ToggleCase => {
                self.case_sensitive = !self.case_sensitive;
                self.current = None;
                SearchReaction::default()
            }
            Message::ToggleRegex => {
                self.regex = !self.regex;
                self.current = None;
                SearchReaction::default()
            }
            Message::Next => {
                let matches = self.find_matches(body.unwrap_or(""));
                SearchReaction {
                    jump_to: self.advance(&matches),
                    ..Default::default()
                }
            }
            Message::Previous => {
                let matches = self.find_matches(body.unwrap_or(""));
                SearchReaction {
                    jump_to: self.retreat(&matches),
                    ..Default::default()
                }
            }
            Message::SetMode(mode) => {
                self.mode = mode;
                SearchReaction::default()
            }
            Message::ReplaceCurrent => {
                let body = body.unwrap_or("");
                let matches = self.find_matches(body);
                // First Replace after typing replaces the first match
                // (treating the unnavigated state as if cursor were at
                // index 0). Subsequent calls replace whatever was
                // highlighted via ›/‹.
                let idx = self.current.unwrap_or(0);
                let Some(m) = matches.get(idx).cloned() else {
                    return SearchReaction::default();
                };
                let expected = body[m.clone()].to_owned();
                let next_jump = matches.get(idx + 1).cloned();
                if next_jump.is_some() {
                    self.current = Some(idx);
                } else {
                    self.current = None;
                }
                SearchReaction {
                    jump_to: next_jump,
                    splices: vec![(m, expected, self.replacement.clone())],
                }
            }
            Message::ReplaceAll => {
                let body = body.unwrap_or("");
                let matches = self.find_matches(body);
                // Highest offset first: applying splices in that order
                // means earlier matches' offsets stay valid against the
                // buffer at the moment each splice happens.
                let splices = matches
                    .into_iter()
                    .rev()
                    .map(|m| {
                        let expected = body[m.clone()].to_owned();
                        (m, expected, self.replacement.clone())
                    })
                    .collect();
                self.current = None;
                SearchReaction { jump_to: None, splices }
            }
        }
    }

    pub(crate) fn view(&self, body: Option<&str>) -> Element<'_, Message> {
        let matches = self.find_matches(body.unwrap_or(""));
        let count = matches.len();

        let count_label = if self.query.is_empty() {
            String::new()
        } else if count == 0 {
            "0 matches".to_owned()
        } else if let Some(i) = self.current {
            // User has navigated — show position so they know where in
            // the result set they are.
            format!("{} / {count}", i + 1)
        } else {
            // Post-type, pre-navigate: matches found but no highlight
            // in the editor yet. Show just the total so the user can
            // see how many results their query has.
            format!("{count} matches")
        };

        let query_input = text_input("Find…", &self.query)
            .on_input(Message::QueryChanged)
            .on_submit(Message::Next)
            .size(12)
            .padding(4);

        let case_btn = button(text("Aa").size(11))
            .on_press(Message::ToggleCase)
            .style(if self.case_sensitive { button::primary } else { button::secondary });
        let regex_btn = button(text(".*").size(11))
            .on_press(Message::ToggleRegex)
            .style(if self.regex { button::primary } else { button::secondary });
        let replace_mode_btn = button(text("\u{2194}").size(11))
            .on_press(Message::SetMode(match self.mode {
                Mode::Find => Mode::Replace,
                Mode::Replace => Mode::Find,
            }))
            .style(if self.mode == Mode::Replace { button::primary } else { button::secondary });

        let prev_btn = button(text("‹").size(11))
            .on_press(Message::Previous)
            .style(button::secondary);
        let next_btn = button(text("›").size(11))
            .on_press(Message::Next)
            .style(button::secondary);

        let mut col = column![
            query_input,
            row![
                case_btn,
                regex_btn,
                replace_mode_btn,
                Space::new().width(Length::Fill),
                text(count_label).size(11),
                prev_btn,
                next_btn,
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(6);

        if self.mode == Mode::Replace {
            // Disable the replace buttons when there are no matches so
            // an accidental click can't trigger a no-op splice attempt.
            let replace_btn = button(text("Replace").size(11))
                .style(button::secondary);
            let replace_btn = if count > 0 {
                replace_btn.on_press(Message::ReplaceCurrent)
            } else {
                replace_btn
            };
            let replace_all_btn = button(text("Replace all").size(11))
                .style(button::secondary);
            let replace_all_btn = if count > 0 {
                replace_all_btn.on_press(Message::ReplaceAll)
            } else {
                replace_all_btn
            };
            col = col
                .push(
                    text_input("Replace with…", &self.replacement)
                        .on_input(Message::ReplacementChanged)
                        .on_submit(Message::ReplaceCurrent)
                        .size(12)
                        .padding(4),
                )
                .push(
                    row![
                        Space::new().width(Length::Fill),
                        replace_btn,
                        replace_all_btn,
                    ]
                    .spacing(4)
                    .align_y(iced::Alignment::Center),
                );
        }

        container(col).padding(12).into()
    }
}

/// Case-insensitive search for short queries when lowering changes byte
/// length. Walks `body` char-by-char comparing lowercased graphemes.
/// Quadratic worst-case but with prose-sized inputs and short queries
/// it doesn't matter — and it's only hit for non-ASCII queries today.
fn fallback_ci_search(body: &str, query: &str) -> Vec<Match> {
    let needle: String = query.to_lowercase();
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < body.len() {
        if !body.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &body[i..];
        let mut consumed = 0usize;
        let mut lowered = String::new();
        for c in rest.chars() {
            consumed += c.len_utf8();
            for lc in c.to_lowercase() {
                lowered.push(lc);
            }
            if lowered.len() >= needle.len() {
                break;
            }
        }
        if lowered.starts_with(&needle) {
            out.push(i..i + consumed);
            i += consumed.max(1);
        } else {
            i += body[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    let _ = bytes; // suppress unused-variable lint without restructuring
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_yields_no_matches() {
        let s = SearchState::new();
        assert!(s.find_matches("anything").is_empty());
    }

    #[test]
    fn case_insensitive_substring_finds_all() {
        let mut s = SearchState::new();
        s.query = "evan".to_owned();
        let body = "Evan and evan and EVAN.";
        let m = s.find_matches(body);
        assert_eq!(m.len(), 3);
        assert_eq!(&body[m[0].clone()], "Evan");
        assert_eq!(&body[m[1].clone()], "evan");
        assert_eq!(&body[m[2].clone()], "EVAN");
    }

    #[test]
    fn case_sensitive_filters_to_exact_form() {
        let mut s = SearchState::new();
        s.query = "Evan".to_owned();
        s.case_sensitive = true;
        let body = "Evan and evan and EVAN.";
        let m = s.find_matches(body);
        assert_eq!(m.len(), 1);
        assert_eq!(&body[m[0].clone()], "Evan");
    }

    #[test]
    fn regex_matches() {
        let mut s = SearchState::new();
        s.query = r"\b[A-Z]\w+".to_owned();
        s.regex = true;
        s.case_sensitive = true;
        let body = "Evan met Aletheia.";
        let m = s.find_matches(body);
        assert_eq!(m.len(), 2);
        assert_eq!(&body[m[0].clone()], "Evan");
        assert_eq!(&body[m[1].clone()], "Aletheia");
    }

    #[test]
    fn invalid_regex_is_silently_zero_matches() {
        let mut s = SearchState::new();
        s.query = "(unclosed".to_owned();
        s.regex = true;
        assert!(s.find_matches("anything").is_empty());
    }

    #[test]
    fn advance_from_fresh_state_lands_on_first_match() {
        // The first Enter / › after typing a query should highlight
        // match #1, not skip to #2 (the old wrap-around semantics).
        let mut s = SearchState::new();
        s.query = "x".to_owned();
        let body = "xxx";
        let matches = s.find_matches(body);
        assert_eq!(s.advance(&matches), Some(0..1));
        assert_eq!(s.advance(&matches), Some(1..2));
        assert_eq!(s.advance(&matches), Some(2..3));
        assert_eq!(s.advance(&matches), Some(0..1), "wraps after last");
    }

    #[test]
    fn retreat_from_fresh_state_lands_on_last_match() {
        // Symmetric: the first ‹ after typing reads as Shift-Tab,
        // landing on the last match rather than skipping it.
        let mut s = SearchState::new();
        s.query = "x".to_owned();
        let body = "xxx";
        let matches = s.find_matches(body);
        assert_eq!(s.retreat(&matches), Some(2..3));
        assert_eq!(s.retreat(&matches), Some(1..2));
        assert_eq!(s.retreat(&matches), Some(0..1));
        assert_eq!(s.retreat(&matches), Some(2..3), "wraps before first");
    }

    #[test]
    fn query_changed_resets_navigation() {
        // Editing the query drops the "current match" pointer so the
        // next Enter starts from the top of the new result set.
        let mut s = SearchState::new();
        s.query = "x".to_owned();
        let body = "xxxy";
        let matches = s.find_matches(body);
        let _ = s.advance(&matches);
        let _ = s.advance(&matches);
        assert_eq!(s.current, Some(1));
        let _ = s.update(Message::QueryChanged("y".to_owned()), Some(body));
        assert_eq!(s.current, None);
        let next = s.update(Message::Next, Some(body));
        assert_eq!(next.jump_to, Some(3..4));
    }

    #[test]
    fn replace_all_orders_splices_highest_offset_first() {
        // Applying the splices in order must not invalidate earlier
        // offsets; the safe ordering is descending. The handler
        // emits exactly that, so callers can iterate and splice
        // without recomputing offsets between writes.
        let mut s = SearchState::new();
        s.query = "x".to_owned();
        s.replacement = "Y".to_owned();
        let reaction = s.update(Message::ReplaceAll, Some("xxx"));
        let offsets: Vec<usize> = reaction.splices.iter().map(|(r, _, _)| r.start).collect();
        assert_eq!(offsets, vec![2, 1, 0]);
        // Each splice carries the original byte slice as "expected" so
        // the editor's drift-guard can verify the offsets still line up.
        for (range, expected, _) in &reaction.splices {
            assert_eq!(expected, &"xxx"[range.clone()]);
        }
    }

    #[test]
    fn replace_current_advances_jump_to_next_match() {
        let mut s = SearchState::new();
        s.query = "x".to_owned();
        s.replacement = "Y".to_owned();
        let reaction = s.update(Message::ReplaceCurrent, Some("xxx"));
        assert_eq!(reaction.splices.len(), 1);
        assert_eq!(reaction.splices[0].0, 0..1);
        // Jump should point at the second match so the user lands on
        // the next candidate after a Replace click.
        assert_eq!(reaction.jump_to, Some(1..2));
    }
}
