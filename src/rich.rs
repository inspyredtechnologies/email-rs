//! Rich text for HTML compose: plain text plus one `Style` per character.
//!
//! The editor edits the plain text; `reconcile` carries styles across each edit,
//! and `to_html` produces the outgoing HTML. Lists are lines prefixed with
//! "• " (bullets) or "1. " (numbering), so they survive switching to plain text.

use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Size {
    Small,
    #[default]
    Normal,
    Large,
    Huge,
}

impl Size {
    pub const ALL: [Size; 4] = [Size::Small, Size::Normal, Size::Large, Size::Huge];

    pub fn label(self) -> &'static str {
        match self {
            Size::Small => "9",
            Size::Normal => "11",
            Size::Large => "14",
            Size::Huge => "18",
        }
    }

    /// Point size used in the outgoing HTML.
    pub fn points(self) -> u8 {
        self.label().parse().unwrap()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub size: Size,
    pub color: Option<[u8; 3]>,
    pub highlight: Option<[u8; 3]>,
}

pub const BULLET: &str = "\u{2022} ";

/// Carry per-character styles across an edit from `old` to `new`.
/// Unchanged text keeps its styles; inserted text gets `typing`.
pub fn reconcile(old: &str, styles: &[Style], new: &str, typing: Style) -> Vec<Style> {
    let old: Vec<char> = old.chars().collect();
    let new: Vec<char> = new.chars().collect();
    let style_at = |i: usize| styles.get(i).copied().unwrap_or_default();
    let max_common = old.len().min(new.len());
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(max_common - prefix)
        .take_while(|(a, b)| a == b)
        .count();
    let inserted = new.len() - prefix - suffix;
    let mut out: Vec<Style> = (0..prefix).map(style_at).collect();
    out.extend(std::iter::repeat_n(typing, inserted));
    out.extend((old.len() - suffix..old.len()).map(style_at));
    out
}

/// Whether every character in `range` satisfies `pred` (false for an empty range).
pub fn all(styles: &[Style], range: Range<usize>, pred: impl Fn(&Style) -> bool) -> bool {
    !range.is_empty() && styles.get(range).is_some_and(|s| s.iter().all(pred))
}

pub fn apply(styles: &mut [Style], range: Range<usize>, f: impl Fn(&mut Style)) {
    let end = range.end.min(styles.len());
    for s in &mut styles[range.start.min(end)..end] {
        f(s);
    }
}

fn number_prefix_len(line: &[char]) -> Option<usize> {
    let digits = line.iter().take_while(|c| c.is_ascii_digit()).count();
    (digits > 0 && line.get(digits) == Some(&'.') && line.get(digits + 1) == Some(&' ')).then_some(digits + 2)
}

fn bullet_prefix_len(line: &[char]) -> Option<usize> {
    line.starts_with(&BULLET.chars().collect::<Vec<_>>()).then_some(2)
}

/// Toggle bullets (or numbering) on every line touched by `range`.
pub fn toggle_list(text: &mut String, styles: &mut Vec<Style>, range: Range<usize>, numbered: bool) {
    let chars: Vec<char> = text.chars().collect();
    styles.resize(chars.len(), Style::default());
    // Split into lines as (start, end) char ranges, excluding the newline.
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, c) in chars.iter().enumerate() {
        if *c == '\n' {
            lines.push((start, i));
            start = i + 1;
        }
    }
    lines.push((start, chars.len()));
    let touched: Vec<bool> = lines
        .iter()
        .map(|&(s, e)| s <= range.end && range.start <= e)
        .collect();
    let has = |l: &[char]| if numbered { number_prefix_len(l) } else { bullet_prefix_len(l) };
    let remove = lines.iter().zip(&touched).filter(|(_, t)| **t).all(|(&(s, e), _)| has(&chars[s..e]).is_some());

    let mut out_chars = Vec::with_capacity(chars.len() + 8);
    let mut out_styles = Vec::with_capacity(chars.len() + 8);
    let mut n = 0;
    for (li, &(s, e)) in lines.iter().enumerate() {
        let line = &chars[s..e];
        let mut body = s;
        if touched[li] {
            // Drop any existing list prefix, then add the new one unless we're removing.
            if let Some(len) = number_prefix_len(line).or_else(|| bullet_prefix_len(line)) {
                body = s + len;
            }
            if !remove {
                n += 1;
                let prefix = if numbered { format!("{n}. ") } else { BULLET.to_string() };
                let style = styles.get(body).copied().unwrap_or_default();
                for c in prefix.chars() {
                    out_chars.push(c);
                    out_styles.push(style);
                }
            }
        }
        out_chars.extend_from_slice(&chars[body..e]);
        out_styles.extend_from_slice(&styles[body..e]);
        if e < chars.len() {
            out_chars.push('\n');
            out_styles.push(styles[e]);
        }
    }
    *text = out_chars.into_iter().collect();
    *styles = out_styles;
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape text and turn bare http(s) URLs into links.
fn linkify(s: &str) -> String {
    let mut out = String::new();
    for (i, word) in s.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let lower = word.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            let url = word.trim_end_matches(['.', ',', ')', ';', '!', '?']);
            let tail = &word[url.len()..];
            out.push_str(&format!("<a href=\"{0}\">{0}</a>{1}", escape(url), escape(tail)));
        } else {
            out.push_str(&escape(word));
        }
    }
    out
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn span(text: &str, s: &Style) -> String {
    let mut css = Vec::new();
    if s.bold {
        css.push("font-weight:bold".to_string());
    }
    if s.italic {
        css.push("font-style:italic".to_string());
    }
    match (s.underline, s.strike) {
        (true, true) => css.push("text-decoration:underline line-through".to_string()),
        (true, false) => css.push("text-decoration:underline".to_string()),
        (false, true) => css.push("text-decoration:line-through".to_string()),
        _ => {}
    }
    if s.size != Size::Normal {
        css.push(format!("font-size:{}pt", s.size.points()));
    }
    if let Some(c) = s.color {
        css.push(format!("color:{}", hex(c)));
    }
    if let Some(c) = s.highlight {
        css.push(format!("background-color:{}", hex(c)));
    }
    let inner = linkify(text);
    if css.is_empty() { inner } else { format!("<span style=\"{}\">{inner}</span>", css.join(";")) }
}

/// Inline HTML for one line of styled characters.
fn line_html(chars: &[char], styles: &[Style]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let style = styles.get(i).copied().unwrap_or_default();
        let mut j = i;
        while j < chars.len() && styles.get(j).copied().unwrap_or_default() == style {
            j += 1;
        }
        let text: String = chars[i..j].iter().collect();
        out.push_str(&span(&text, &style));
        i = j;
    }
    // Keep runs of spaces visible like the editor does.
    out.replace("  ", " &nbsp;")
}

/// Convert styled text to HTML: one `<div>` per line, lists as `<ul>`/`<ol>`.
pub fn to_html(text: &str, styles: &[Style]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut open_list: Option<&str> = None;
    let mut start = 0;
    for end in (0..=chars.len()).filter(|&i| i == chars.len() || chars[i] == '\n') {
        let line = &chars[start..end];
        let line_styles = styles.get(start..end).unwrap_or(&[]);
        let (kind, skip) = match (bullet_prefix_len(line), number_prefix_len(line)) {
            (Some(n), _) => (Some("ul"), n),
            (_, Some(n)) => (Some("ol"), n),
            _ => (None, 0),
        };
        if open_list != kind {
            if let Some(k) = open_list {
                out.push_str(&format!("</{k}>"));
            }
            if let Some(k) = kind {
                out.push_str(&format!("<{k} style=\"margin-top:0;margin-bottom:0\">"));
            }
            open_list = kind;
        }
        let inner = line_html(&line[skip..], line_styles.get(skip..).unwrap_or(&[]));
        match kind {
            Some(_) => out.push_str(&format!("<li>{inner}</li>")),
            None if inner.is_empty() => out.push_str("<div><br></div>"),
            None => out.push_str(&format!("<div>{inner}</div>")),
        }
        start = end + 1;
    }
    if let Some(k) = open_list {
        out.push_str(&format!("</{k}>"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bold() -> Style {
        Style { bold: true, ..Default::default() }
    }

    #[test]
    fn reconcile_keeps_styles_around_edits() {
        let styles = vec![bold(), bold(), Style::default()];
        // Insert in the middle: new chars take the typing style.
        let out = reconcile("abc", &styles, "abXc", Style::default());
        assert_eq!(out, vec![bold(), bold(), Style::default(), Style::default()]);
        // Delete a bold char.
        assert_eq!(reconcile("abc", &styles, "ac", bold()), vec![bold(), Style::default()]);
        // Replace a selection.
        assert_eq!(reconcile("abc", &styles, "aZZ", bold()), vec![bold(), bold(), bold()]);
        // Repeated characters don't confuse the diff.
        assert_eq!(reconcile("aa", &[bold(), Style::default()], "aaa", bold()).len(), 3);
    }

    #[test]
    fn toggles_bullets_and_numbering() {
        let mut text = "one\ntwo\nthree".to_string();
        let mut styles = vec![Style::default(); text.chars().count()];
        toggle_list(&mut text, &mut styles, 0..6, false);
        assert_eq!(text, "\u{2022} one\n\u{2022} two\nthree");
        assert_eq!(styles.len(), text.chars().count());
        toggle_list(&mut text, &mut styles, 0..8, true);
        assert_eq!(text, "1. one\n2. two\nthree");
        toggle_list(&mut text, &mut styles, 0..8, true);
        assert_eq!(text, "one\ntwo\nthree");
    }

    #[test]
    fn exports_html() {
        let text = "Hi <team>\n\n\u{2022} see https://contoso.example/x.\n\u{2022} done";
        let mut styles = vec![Style::default(); text.chars().count()];
        apply(&mut styles, 0..2, |s| s.bold = true);
        let html = to_html(text, &styles);
        assert_eq!(
            html,
            "<div><span style=\"font-weight:bold\">Hi</span> &lt;team&gt;</div><div><br></div>\
             <ul style=\"margin-top:0;margin-bottom:0\"><li>see <a href=\"https://contoso.example/x\">https://contoso.example/x</a>.</li>\
             <li>done</li></ul>"
        );
        let mut styles = vec![Style::default(); 3];
        apply(&mut styles, 0..3, |s| {
            s.color = Some([0xC0, 0, 0]);
            s.size = Size::Large;
        });
        assert_eq!(to_html("red", &styles), "<div><span style=\"font-size:14pt;color:#C00000\">red</span></div>");
    }
}
