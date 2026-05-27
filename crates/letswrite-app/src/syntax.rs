//! Markdown syntax highlighter for the editor pane.
//!
//! Implements [`iced::advanced::text::Highlighter`] over Markdown. Operates
//! line-by-line; the only cross-line state is "are we inside a fenced code
//! block" (and the frontmatter block at the top of the file).
//!
//! Iced's `text_editor` cannot vary font *size* per highlight span — only
//! color and font (which carries weight). So hierarchy is conveyed via color
//! + bold, not size. Themes are designed with this constraint in mind.

use std::ops::Range;

use iced::advanced::text::Highlighter;
use iced::advanced::text::highlighter::Format;
use iced::{Color, Font};

/// What kind of Markdown token a span belongs to. Picked so each kind maps to
/// exactly one [`Format`] in any theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    /// Frontmatter line (`---` delimiter or YAML inside).
    Frontmatter,
    /// Heading marker (`#`, `##`, …) — color graded by level.
    HeadingMarker(u8),
    /// Heading text — same color as marker but always bold.
    HeadingText(u8),
    /// `**bold**` markers and text inside (bold weight).
    Bold,
    /// `*italic*` or `_italic_` markers and text inside (italic weight).
    Italic,
    /// `` `inline code` `` including the backticks.
    InlineCode,
    /// ```` ``` ```` fence line.
    CodeFence,
    /// Body of a fenced code block.
    CodeBlock,
    /// `> ` blockquote marker (the prefix only; quoted text stays plain so
    /// it remains comfortable to read at length).
    BlockquoteMarker,
    /// List marker (`-`, `*`, `+`, `1.`, `2.`) at the start of a line.
    ListMarker,
    /// `[link text](url)` — the bracketed text.
    LinkText,
    /// `[link text](url)` — the URL part.
    LinkUrl,
    /// `[[wiki link]]` — the whole thing.
    WikiLink,
    /// `#hashtag` style tag (Obsidian-style, single `#` followed by non-space).
    /// We distinguish from heading by requiring it to not be at column 0
    /// followed by space.
    Hashtag,
}

/// User-selectable syntax theme. Color schemes share the same role mapping;
/// only the palette changes. Persisted via [`letswrite_core::settings::SyntaxTheme`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) enum SyntaxTheme {
    /// Okabe-Ito palette — designed for color-blind viewers. Default because
    /// it works for everyone, not just trichromats.
    #[default]
    ColorblindSafe,
    /// Warm, low-contrast palette inspired by Solarized.
    Solarized,
    /// Saturated white/cyan/amber on dark — strongest separation for scanning.
    HighContrast,
}

impl SyntaxTheme {
    // `ALL` and `label` will be used by the settings UI in a future task
    // — kept here so the theme list is in one place.
    #[allow(dead_code)]
    pub(crate) const ALL: [Self; 3] =
        [Self::ColorblindSafe, Self::Solarized, Self::HighContrast];

    #[allow(dead_code)]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ColorblindSafe => "Color-blind safe (Okabe-Ito)",
            Self::Solarized => "Solarized",
            Self::HighContrast => "High contrast",
        }
    }

    pub(crate) const fn from_settings(s: letswrite_core::settings::SyntaxTheme) -> Self {
        use letswrite_core::settings::SyntaxTheme as S;
        match s {
            S::ColorblindSafe => Self::ColorblindSafe,
            S::Solarized => Self::Solarized,
            S::HighContrast => Self::HighContrast,
        }
    }

    /// Map a token kind to a concrete [`Format`] under this theme.
    pub(crate) fn format_for(self, kind: TokenKind) -> Format<Font> {
        let palette = self.palette();
        let (color, bold, italic) = match kind {
            TokenKind::Frontmatter
            | TokenKind::CodeFence
            | TokenKind::BlockquoteMarker
            | TokenKind::LinkUrl => (palette.muted, false, false),
            TokenKind::HeadingMarker(level) => (palette.heading(level), false, false),
            TokenKind::HeadingText(level) => (palette.heading(level), true, false),
            TokenKind::Bold => (palette.emphasis_strong, true, false),
            TokenKind::Italic => (palette.emphasis_strong, false, true),
            TokenKind::InlineCode | TokenKind::CodeBlock => (palette.code, false, false),
            TokenKind::ListMarker => (palette.marker, true, false),
            TokenKind::LinkText | TokenKind::WikiLink => (palette.link, false, false),
            TokenKind::Hashtag => (palette.tag, false, false),
        };
        Format {
            color: Some(color),
            font: Some(weighted_font(bold, italic)),
        }
    }

    const fn palette(self) -> Palette {
        match self {
            // Okabe-Ito (2008) palette, picked specifically for safe distinction
            // under all common color-vision deficiencies. Reordered slightly so
            // heading H1 lands on the most prominent hue.
            Self::ColorblindSafe => Palette {
                heading_levels: [
                    rgb(0xE6, 0x9F, 0x00), // H1: orange
                    rgb(0x56, 0xB4, 0xE9), // H2: sky blue
                    rgb(0x00, 0x9E, 0x73), // H3: bluish green
                    rgb(0xCC, 0x79, 0xA7), // H4: reddish purple
                    rgb(0xF0, 0xE4, 0x42), // H5: yellow
                    rgb(0x00, 0x72, 0xB2), // H6: blue
                ],
                emphasis_strong: rgb(0xE6, 0x9F, 0x00),
                code: rgb(0x00, 0x9E, 0x73),
                muted: rgb(0x80, 0x80, 0x80),
                marker: rgb(0x56, 0xB4, 0xE9),
                link: rgb(0x00, 0x72, 0xB2),
                tag: rgb(0xCC, 0x79, 0xA7),
            },
            Self::Solarized => Palette {
                heading_levels: [
                    rgb(0xCB, 0x4B, 0x16), // H1: orange
                    rgb(0xD3, 0x36, 0x82), // H2: magenta
                    rgb(0x26, 0x8B, 0xD2), // H3: blue
                    rgb(0x2A, 0xA1, 0x98), // H4: cyan
                    rgb(0x85, 0x99, 0x00), // H5: green
                    rgb(0x6C, 0x71, 0xC4), // H6: violet
                ],
                emphasis_strong: rgb(0xCB, 0x4B, 0x16),
                code: rgb(0x2A, 0xA1, 0x98),
                muted: rgb(0x65, 0x7B, 0x83),
                marker: rgb(0xB5, 0x89, 0x00),
                link: rgb(0x26, 0x8B, 0xD2),
                tag: rgb(0xD3, 0x36, 0x82),
            },
            Self::HighContrast => Palette {
                heading_levels: [
                    rgb(0xFF, 0xFF, 0xFF),
                    rgb(0x00, 0xE5, 0xFF),
                    rgb(0xFF, 0xC1, 0x07),
                    rgb(0xFF, 0x6E, 0x40),
                    rgb(0x7C, 0xFF, 0xB2),
                    rgb(0xBB, 0xDE, 0xFB),
                ],
                emphasis_strong: rgb(0xFF, 0xC1, 0x07),
                code: rgb(0x7C, 0xFF, 0xB2),
                muted: rgb(0x90, 0x90, 0x90),
                marker: rgb(0x00, 0xE5, 0xFF),
                link: rgb(0xFF, 0xC1, 0x07),
                tag: rgb(0xFF, 0x6E, 0x40),
            },
        }
    }
}

/// Concrete colors for one syntax theme. Heading levels indexed 0..=5 for
/// H1..=H6.
#[derive(Debug, Clone, Copy)]
struct Palette {
    heading_levels: [Color; 6],
    emphasis_strong: Color,
    code: Color,
    muted: Color,
    marker: Color,
    link: Color,
    tag: Color,
}

impl Palette {
    fn heading(&self, level: u8) -> Color {
        let idx = (level.max(1) - 1).min(5) as usize;
        self.heading_levels[idx]
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

const fn weighted_font(bold: bool, italic: bool) -> Font {
    use iced::font::{Style, Weight};
    Font {
        weight: if bold { Weight::Bold } else { Weight::Normal },
        style: if italic { Style::Italic } else { Style::Normal },
        ..Font::MONOSPACE
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    pub theme: SyntaxTheme,
}

/// Cross-line state — only what changes between lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineState {
    /// Top of file: the very next non-empty line might open a frontmatter block.
    Initial,
    /// Inside the frontmatter block (between `---` delimiters at file start).
    InFrontmatter,
    /// Normal Markdown body.
    Body,
    /// Inside a fenced code block (between matching ` ``` ` delimiters).
    InCodeFence,
}

pub(crate) struct MarkdownHighlighter {
    settings: Settings,
    line_states: Vec<LineState>,
    current_line: usize,
}

impl std::fmt::Debug for MarkdownHighlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkdownHighlighter")
            .field("theme", &self.settings.theme)
            .field("current_line", &self.current_line)
            .field("lines_cached", &self.line_states.len())
            .finish()
    }
}

/// What the editor's format function receives per span. The theme is
/// carried alongside the kind because the format function is a plain `fn`
/// pointer (can't capture state) — see [`iced::widget::text_editor::TextEditor::highlight_with`].
pub(crate) type Highlight = (TokenKind, SyntaxTheme);

impl Highlighter for MarkdownHighlighter {
    type Settings = Settings;
    type Highlight = Highlight;

    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Highlight)>;

    fn new(settings: &Self::Settings) -> Self {
        Self {
            settings: settings.clone(),
            line_states: vec![LineState::Initial],
            current_line: 0,
        }
    }

    fn update(&mut self, new_settings: &Self::Settings) {
        if &self.settings != new_settings {
            self.settings = new_settings.clone();
            // Theme change is purely cosmetic — no need to invalidate state.
        }
    }

    fn change_line(&mut self, line: usize) {
        // Discard cached state from `line` onwards; subsequent highlight_line
        // calls will rebuild it.
        self.line_states.truncate(line + 1);
        self.current_line = line;
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        // The state at the start of this line is the cached state for index
        // `current_line` (or Initial if we've never seen this line).
        let entry_state = self
            .line_states
            .get(self.current_line)
            .copied()
            .unwrap_or(LineState::Initial);
        let (tokens, exit_state) = scan_line(line, entry_state);
        // Cache the *exit* state at index current_line + 1.
        let next_idx = self.current_line + 1;
        if next_idx >= self.line_states.len() {
            self.line_states.resize(next_idx + 1, LineState::Body);
        }
        self.line_states[next_idx] = exit_state;
        self.current_line += 1;
        let theme = self.settings.theme;
        tokens
            .into_iter()
            .map(|(r, k)| (r, (k, theme)))
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn current_line(&self) -> usize {
        self.current_line
    }
}

/// Scan one line, producing tokens and the line-state at the line's end.
fn scan_line(line: &str, entry: LineState) -> (Vec<(Range<usize>, TokenKind)>, LineState) {
    let mut tokens: Vec<(Range<usize>, TokenKind)> = Vec::new();

    // --- Frontmatter handling ----------------------------------------------
    if entry == LineState::Initial {
        if line.trim_end_matches('\r') == "---" {
            tokens.push((0..line.len(), TokenKind::Frontmatter));
            return (tokens, LineState::InFrontmatter);
        }
        // No frontmatter — fall through to Body handling.
        return scan_body_line(line);
    }
    if entry == LineState::InFrontmatter {
        tokens.push((0..line.len(), TokenKind::Frontmatter));
        let next = if line.trim_end_matches('\r') == "---" {
            LineState::Body
        } else {
            LineState::InFrontmatter
        };
        return (tokens, next);
    }

    // --- Fenced code block --------------------------------------------------
    if entry == LineState::InCodeFence {
        if is_code_fence(line) {
            tokens.push((0..line.len(), TokenKind::CodeFence));
            return (tokens, LineState::Body);
        }
        tokens.push((0..line.len(), TokenKind::CodeBlock));
        return (tokens, LineState::InCodeFence);
    }

    // --- Body --------------------------------------------------------------
    scan_body_line(line)
}

/// Scan a normal body line — never inside frontmatter, never inside a fence.
/// The returned exit state is either `Body` or `InCodeFence`.
fn scan_body_line(line: &str) -> (Vec<(Range<usize>, TokenKind)>, LineState) {
    let mut tokens: Vec<(Range<usize>, TokenKind)> = Vec::new();

    // Code fence opens.
    if is_code_fence(line) {
        tokens.push((0..line.len(), TokenKind::CodeFence));
        return (tokens, LineState::InCodeFence);
    }

    // Heading: 1-6 `#`s then a space.
    if let Some((level, marker_end)) = parse_heading_marker(line) {
        tokens.push((0..marker_end, TokenKind::HeadingMarker(level)));
        if marker_end < line.len() {
            tokens.push((marker_end..line.len(), TokenKind::HeadingText(level)));
        }
        return (tokens, LineState::Body);
    }

    // Blockquote: `>` (optionally after whitespace), then optional space,
    // then arbitrary content. We mark only the marker; the quoted text reads
    // as plain so writers don't fight the eye over long quotes.
    if let Some(marker_end) = parse_blockquote_marker(line) {
        tokens.push((0..marker_end, TokenKind::BlockquoteMarker));
        // Now scan inline tokens in the remainder.
        scan_inline(&line[marker_end..], marker_end, &mut tokens);
        return (tokens, LineState::Body);
    }

    // List marker: `- `, `* `, `+ `, or `<digits>. `.
    if let Some(marker_end) = parse_list_marker(line) {
        tokens.push((0..marker_end, TokenKind::ListMarker));
        scan_inline(&line[marker_end..], marker_end, &mut tokens);
        return (tokens, LineState::Body);
    }

    // Plain paragraph — just scan inline spans.
    scan_inline(line, 0, &mut tokens);
    (tokens, LineState::Body)
}

fn is_code_fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

fn parse_heading_marker(line: &str) -> Option<(u8, usize)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] == b'#' && i < 6 {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    // Must be followed by space (or end of line for an empty heading).
    if i < bytes.len() && bytes[i] != b' ' {
        return None;
    }
    // Include the trailing space in the marker so the inline scan starts on
    // actual content.
    let marker_end = if i < bytes.len() { i + 1 } else { i };
    #[allow(clippy::cast_possible_truncation)]
    Some((i as u8, marker_end))
}

fn parse_blockquote_marker(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    // Allow up to three leading spaces, per CommonMark.
    while i < 3 && i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'>' {
        return None;
    }
    i += 1;
    if i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    Some(i)
}

fn parse_list_marker(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    // Indentation tolerance.
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    let marker_start = i;
    if i >= bytes.len() {
        return None;
    }
    match bytes[i] {
        b'-' | b'*' | b'+' => {
            i += 1;
            if i >= bytes.len() || bytes[i] != b' ' {
                return None;
            }
            i += 1;
        }
        b'0'..=b'9' => {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            // Allow `.` or `)` after the number, per CommonMark.
            if i >= bytes.len() || (bytes[i] != b'.' && bytes[i] != b')') {
                return None;
            }
            i += 1;
            if i >= bytes.len() || bytes[i] != b' ' {
                return None;
            }
            i += 1;
        }
        _ => return None,
    }
    // Sanity: a list marker without an indent that's bare `* ` could be
    // ambiguous with bold-start; require at least one char after the marker
    // to consider it a list. Empty list lines (`- \n`) are still lists.
    let _ = marker_start;
    Some(i)
}

/// Scan inline tokens on a single (sub)line. `offset` is the offset of `s`
/// inside the original line — every emitted range is shifted accordingly.
fn scan_inline(
    s: &str,
    offset: usize,
    out: &mut Vec<(Range<usize>, TokenKind)>,
) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        // Wiki link: `[[...]]`
        if i + 1 < n && bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(end) = find_double_close(s, i + 2) {
                out.push((offset + i..offset + end, TokenKind::WikiLink));
                i = end;
                continue;
            }
        }

        // Standard link: `[text](url)` — only highlight when the full shape
        // is present, otherwise leave alone (tolerates mid-edit).
        if bytes[i] == b'[' {
            if let Some((text_end, url_end)) = parse_link(s, i) {
                out.push((offset + i + 1..offset + text_end, TokenKind::LinkText));
                out.push((
                    offset + text_end + 1..offset + url_end,
                    TokenKind::LinkUrl,
                ));
                i = url_end + 1; // past the closing `)`
                continue;
            }
        }

        // Inline code: `` `...` ``  (single backticks).
        if bytes[i] == b'`' {
            if let Some(end) = s[i + 1..].find('`').map(|p| i + 1 + p + 1) {
                out.push((offset + i..offset + end, TokenKind::InlineCode));
                i = end;
                continue;
            }
        }

        // Bold: `**...**`
        if i + 1 < n && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            if let Some(end) = find_two(s, i + 2, b'*', b'*') {
                out.push((offset + i..offset + end, TokenKind::Bold));
                i = end;
                continue;
            }
        }

        // Italic: `*...*` (non-greedy; not `**`) or `_..._`.
        if (bytes[i] == b'*' || bytes[i] == b'_')
            && !is_word_char(bytes, i.wrapping_sub(1))
        {
            let marker = bytes[i];
            // Skip if this is actually a bold marker.
            if marker == b'*' && i + 1 < n && bytes[i + 1] == b'*' {
                i += 1;
                continue;
            }
            if let Some(end) = s[i + 1..].find(marker as char).map(|p| i + 1 + p + 1) {
                // Refuse empty `**` or `__` runs that snuck through.
                if end > i + 1 {
                    out.push((offset + i..offset + end, TokenKind::Italic));
                    i = end;
                    continue;
                }
            }
        }

        // Hashtag: `#word` not at column 0 (col-0 with following space is
        // already eaten as a heading by scan_body_line).
        if bytes[i] == b'#'
            && (i == 0 || !is_word_char(bytes, i - 1))
            && i + 1 < n
            && is_hashtag_char(bytes[i + 1])
        {
            let mut end = i + 1;
            while end < n && is_hashtag_char(bytes[end]) {
                end += 1;
            }
            // Skip ATX-heading lookalikes: if this is column 0 and followed
            // by a space, scan_body_line would've classified it. Defense in
            // depth.
            if !(i == 0 && end < n && bytes[end] == b' ') {
                out.push((offset + i..offset + end, TokenKind::Hashtag));
                i = end;
                continue;
            }
        }

        i += 1;
    }
}

fn find_double_close(s: &str, from: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = from;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b']' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// Parse `[text](url)` starting at the `[`. Returns `(text_end_idx (the ])`
/// position, `url_end_idx` (the `)` position)).
fn parse_link(s: &str, start: usize) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let text_end = s[start + 1..].find(']')? + start + 1;
    if bytes.get(text_end + 1) != Some(&b'(') {
        return None;
    }
    let url_end = s[text_end + 2..].find(')')? + text_end + 2;
    Some((text_end, url_end))
}

fn find_two(s: &str, from: usize, a: u8, b: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = from;
    while i + 1 < bytes.len() {
        if bytes[i] == a && bytes[i + 1] == b {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

fn is_word_char(bytes: &[u8], i: usize) -> bool {
    if i >= bytes.len() {
        return false;
    }
    let c = bytes[i];
    c.is_ascii_alphanumeric() || c == b'_'
}

const fn is_hashtag_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'/'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(line: &str) -> Vec<(Range<usize>, TokenKind)> {
        scan_body_line(line).0
    }

    #[test]
    fn heading_markers_are_tagged_by_level() {
        let toks = scan("## A heading");
        assert_eq!(
            toks,
            vec![
                (0..3, TokenKind::HeadingMarker(2)),
                (3..12, TokenKind::HeadingText(2)),
            ]
        );
    }

    #[test]
    fn h6_max_then_falls_back_to_plain() {
        // 7 # should NOT count as a heading.
        let toks = scan("####### too many");
        assert!(toks.iter().all(|(_, k)| !matches!(k, TokenKind::HeadingMarker(_))));
    }

    #[test]
    fn bold_and_italic_in_one_line() {
        let line = "This is **bold** and *italic* text.";
        let toks = scan(line);
        assert!(toks
            .iter()
            .any(|(r, k)| matches!(k, TokenKind::Bold)
                && &line[r.clone()] == "**bold**"));
        assert!(toks
            .iter()
            .any(|(r, k)| matches!(k, TokenKind::Italic)
                && &line[r.clone()] == "*italic*"));
    }

    #[test]
    fn unclosed_emphasis_does_not_panic() {
        // Mid-edit: writer just typed `**` and is about to type the rest.
        let _ = scan("starting **a bold word but no close");
        let _ = scan("`unfinished code");
        let _ = scan("[[wiki without close");
    }

    #[test]
    fn inline_code_and_blockquote() {
        let toks = scan("> here is `code` inside a quote");
        assert!(toks
            .iter()
            .any(|(_, k)| matches!(k, TokenKind::BlockquoteMarker)));
        assert!(toks.iter().any(|(_, k)| matches!(k, TokenKind::InlineCode)));
    }

    #[test]
    fn wiki_link_detected() {
        let line = "talked with [[Evan Calder]] today";
        let toks = scan(line);
        let wiki: Vec<_> = toks
            .iter()
            .filter(|(_, k)| matches!(k, TokenKind::WikiLink))
            .collect();
        assert_eq!(wiki.len(), 1);
        assert_eq!(&line[wiki[0].0.clone()], "[[Evan Calder]]");
    }

    #[test]
    fn markdown_link_text_and_url_split() {
        let line = "see [the docs](https://example.com) please";
        let toks = scan(line);
        let text = toks.iter().find(|(_, k)| matches!(k, TokenKind::LinkText));
        let url = toks.iter().find(|(_, k)| matches!(k, TokenKind::LinkUrl));
        assert!(text.is_some(), "link text should be tagged");
        assert!(url.is_some(), "link url should be tagged");
    }

    #[test]
    fn list_markers_for_each_form() {
        for line in ["- one", "* two", "+ three", "1. four", "  - indented", "12) ord"] {
            let toks = scan(line);
            assert!(
                toks.iter().any(|(_, k)| matches!(k, TokenKind::ListMarker)),
                "missing list marker for: {line:?}"
            );
        }
    }

    #[test]
    fn hashtag_recognized_in_prose_not_at_column_zero_with_space() {
        let line = "the agenda is #threshold and not @other";
        let toks = scan(line);
        assert!(toks.iter().any(|(_, k)| matches!(k, TokenKind::Hashtag)));
    }

    #[test]
    fn col_zero_hash_space_is_heading_not_tag() {
        let toks = scan("# Heading");
        assert!(toks.iter().any(|(_, k)| matches!(k, TokenKind::HeadingMarker(1))));
        assert!(toks.iter().all(|(_, k)| !matches!(k, TokenKind::Hashtag)));
    }

    #[test]
    fn frontmatter_lines_cross_state_transitions() {
        let mut h = MarkdownHighlighter::new(&Settings {
            theme: SyntaxTheme::default(),
        });
        let l1: Vec<_> = h.highlight_line("---").collect();
        let l2: Vec<_> = h.highlight_line("title: t").collect();
        let l3: Vec<_> = h.highlight_line("---").collect();
        let l4: Vec<_> = h.highlight_line("# Chapter").collect();
        assert!(l1.iter().any(|(_, (k, _))| matches!(k, TokenKind::Frontmatter)));
        assert!(l2.iter().any(|(_, (k, _))| matches!(k, TokenKind::Frontmatter)));
        assert!(l3.iter().any(|(_, (k, _))| matches!(k, TokenKind::Frontmatter)));
        assert!(l4.iter().any(|(_, (k, _))| matches!(k, TokenKind::HeadingMarker(1))));
    }

    #[test]
    fn fenced_code_block_spans_lines() {
        let mut h = MarkdownHighlighter::new(&Settings {
            theme: SyntaxTheme::default(),
        });
        // Body first to skip the initial-frontmatter check cleanly.
        let _ = h.highlight_line("intro");
        let l_open: Vec<_> = h.highlight_line("```rust").collect();
        let l_inner: Vec<_> = h.highlight_line("fn main() {}").collect();
        let l_close: Vec<_> = h.highlight_line("```").collect();
        assert!(l_open.iter().any(|(_, (k, _))| matches!(k, TokenKind::CodeFence)));
        assert!(l_inner.iter().any(|(_, (k, _))| matches!(k, TokenKind::CodeBlock)));
        assert!(l_close.iter().any(|(_, (k, _))| matches!(k, TokenKind::CodeFence)));
    }

    #[test]
    fn change_line_invalidates_subsequent_state() {
        let mut h = MarkdownHighlighter::new(&Settings {
            theme: SyntaxTheme::default(),
        });
        let _ = h.highlight_line("---").count();
        let _ = h.highlight_line("title: t").count();
        // Pretend the user changed line 0. We should be able to start over.
        h.change_line(0);
        let again: Vec<_> = h.highlight_line("---").collect();
        assert!(again.iter().any(|(_, (k, _))| matches!(k, TokenKind::Frontmatter)));
    }

    #[test]
    fn all_themes_produce_a_format_for_each_kind() {
        // Smoke test: every theme handles every kind without panicking.
        let kinds = [
            TokenKind::Frontmatter,
            TokenKind::HeadingMarker(1),
            TokenKind::HeadingMarker(6),
            TokenKind::HeadingText(3),
            TokenKind::Bold,
            TokenKind::Italic,
            TokenKind::InlineCode,
            TokenKind::CodeFence,
            TokenKind::CodeBlock,
            TokenKind::BlockquoteMarker,
            TokenKind::ListMarker,
            TokenKind::LinkText,
            TokenKind::LinkUrl,
            TokenKind::WikiLink,
            TokenKind::Hashtag,
        ];
        for theme in SyntaxTheme::ALL {
            for kind in kinds {
                let _ = theme.format_for(kind);
            }
        }
    }
}
