//! Color, text measurement, time formatting, and the plain (non-interactive) list.
//!
//! Widths are counted in chars, exactly as the JS original counted UTF-16 units:
//! no display-width table is consulted, so a wide CJK label can overhang by design
//! rather than by accident.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::model::{short_id, Node, Row};

// ---------- colors ----------
// Set once in main() from the --color/NO_COLOR/TTY decision; the helpers below read
// it at call time, like the module-level flag in the JS version.
static USE_COLOR: AtomicBool = AtomicBool::new(false);

pub fn set_color(on: bool) {
    USE_COLOR.store(on, Ordering::Relaxed);
}
pub fn use_color() -> bool {
    USE_COLOR.load(Ordering::Relaxed)
}
fn c(code: &str, s: &str) -> String {
    if use_color() {
        format!("\x1b[{}m{}\x1b[0m", code, s)
    } else {
        s.to_string()
    }
}
pub fn dim(s: &str) -> String {
    c("2", s)
}
pub fn bold(s: &str) -> String {
    c("1", s)
}
pub fn cyan(s: &str) -> String {
    c("36", s)
}
pub fn green(s: &str) -> String {
    c("32", s)
}
pub fn yellow(s: &str) -> String {
    c("33", s)
}

// ---------- text ----------

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Keep `n` chars, marking a cut with an ellipsis.
pub fn truncate(s: &str, n: usize) -> String {
    if char_len(s) > n {
        if n == 0 {
            return String::new();
        }
        let mut out: String = s.chars().take(n - 1).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

/// Hard cut at `n` chars, no ellipsis.
pub fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Pad with spaces to `w` chars. Like JS padEnd, this counts any ANSI escapes in `s`
/// as characters; callers that pad colored text compensate for that themselves.
pub fn pad_end(s: &str, w: usize) -> String {
    let len = char_len(s);
    if len >= w {
        s.to_string()
    } else {
        let mut out = s.to_string();
        out.extend(std::iter::repeat(' ').take(w - len));
        out
    }
}

/// Greedy word wrap, hard-breaking any word longer than `width`.
pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    if width == 0 {
        return out;
    }
    for w in s.split_whitespace() {
        let mut word: String = w.to_string();
        while char_len(&word) > width {
            if !line.is_empty() {
                out.push(line.clone());
                line.clear();
            }
            out.push(clip(&word, width));
            word = word.chars().skip(width).collect();
        }
        if line.is_empty() {
            line = word;
        } else if char_len(&line) + 1 + char_len(&word) <= width {
            line.push(' ');
            line.push_str(&word);
        } else {
            out.push(line.clone());
            line = word;
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

// ---------- time ----------

/// Full last-active stamp for the detail panel, e.g. `7/15/2026, 3:42:18 PM`. The JS
/// original delegated to `toLocaleString()`; this is that format pinned down, so the
/// output does not shift with the machine's locale.
pub fn format_time(ms: i64) -> String {
    match crate::term::local_time(ms) {
        Some(t) => {
            let h = if t.hour % 12 == 0 { 12 } else { t.hour % 12 };
            format!(
                "{}/{}/{}, {}:{:02}:{:02} {}",
                t.mon,
                t.mday,
                t.year,
                h,
                t.min,
                t.sec,
                if t.hour < 12 { "AM" } else { "PM" }
            )
        }
        None => String::new(),
    }
}

/// Short last-active stamp for the picker's tag column, e.g. `15/08 1:05PM`. Built by
/// hand rather than from a locale so the format is identical on every machine. Day and
/// month are zero-padded; the hour is not, so the column is 12 or 13 chars wide.
pub fn short_time(ms: i64) -> String {
    if ms == 0 {
        return String::new();
    }
    match crate::term::local_time(ms) {
        Some(t) => {
            let h = if t.hour % 12 == 0 { 12 } else { t.hour % 12 };
            format!(
                "{:02}/{:02} {}:{:02}{}",
                t.mday,
                t.mon,
                h,
                t.min,
                if t.hour < 12 { "AM" } else { "PM" }
            )
        }
        None => String::new(),
    }
}

// ---------- list view ----------

pub fn resume_line(id: &str) -> String {
    format!("/resume {}", id)
}

pub struct Meta {
    /// The current session belongs to a different project than the one being shown.
    pub elsewhere: bool,
    pub current_id: Option<String>,
}

/// The plain list: what you get piped, with --list, or inside Claude Code's `!`.
pub fn render_list(nodes: &[Node], rows: &[Row], project_name: &str, meta: &Meta) -> String {
    let idx_w = rows.len().to_string().len();
    // Label column width, for alignment.
    let tree_w = rows
        .iter()
        .map(|r| char_len(&format!("{}{}●", r.prefix, r.connector)))
        .max()
        .unwrap_or(0);
    let label_w = std::cmp::min(
        40,
        rows.iter()
            .map(|r| char_len(&truncate(&nodes[r.node].label, 40)))
            .max()
            .unwrap_or(0),
    );

    let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 4);
    lines.push(bold(&format!("Branches in {}:", project_name)));
    for r in rows {
        let n = &nodes[r.node];
        let idx = format!("{:>width$}", r.index, width = idx_w);
        let glyph = if n.current {
            green("●")
        } else if n.latest {
            yellow("●")
        } else {
            "●".to_string()
        };
        let highlighted = n.current || n.latest;
        // A colored glyph carries 9 extra chars of escape codes that padEnd counts but
        // the terminal does not draw, so widen the pad by exactly that much.
        let tree = pad_end(
            &format!("{}{}{}", r.prefix, r.connector, glyph),
            tree_w + if use_color() && highlighted { 9 } else { 0 },
        );
        let sid = cyan(short_id(&n.session_id));
        // A title or name is shown bold; a first prompt is shown as plain text.
        let plain = pad_end(&truncate(&n.label, 40), label_w);
        let label = if n.strong { bold(&plain) } else { plain };
        let action = if n.current {
            yellow("← current (this session)")
        } else {
            let mut a = dim(&resume_line(&n.session_id));
            if n.latest {
                a.push(' ');
                a.push_str(&yellow("(most recent)"));
            }
            a
        };
        lines.push(format!(
            "  {}  {} {}  {}  {}",
            idx, tree, sid, label, action
        ));
    }
    lines.push(String::new());
    if meta.elsewhere {
        if let Some(id) = meta.current_id.as_deref() {
            lines.push(dim(&format!(
                "Current session {} belongs to another project; marking the most recent here.",
                short_id(id)
            )));
        }
    }
    lines.push(dim(
        "Paste a /resume line to switch in place. Or run `!branch-graph <n>` for one line.",
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_marks_the_cut_and_counts_chars() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abcd", 4), "abcd");
        assert_eq!(truncate("", 4), "");
        // Counted in chars, not bytes, so multi-byte labels are not cut mid-character.
        assert_eq!(truncate("日本語です", 3), "日本…");
        assert_eq!(truncate("日本", 3), "日本");
    }

    #[test]
    fn clip_is_a_hard_cut() {
        assert_eq!(clip("abcdef", 3), "abc");
        assert_eq!(clip("日本語", 2), "日本");
        assert_eq!(clip("ab", 5), "ab");
    }

    #[test]
    fn pad_end_pads_to_char_width() {
        assert_eq!(pad_end("ab", 4), "ab  ");
        assert_eq!(pad_end("abcd", 2), "abcd");
        assert_eq!(char_len(&pad_end("日本", 4)), 4);
    }

    #[test]
    fn wrap_text_wraps_and_hard_breaks() {
        assert_eq!(
            wrap_text("the quick brown fox jumps", 11),
            vec!["the quick", "brown fox", "jumps"]
        );
        // A word longer than the width is broken rather than overflowing the panel.
        assert_eq!(
            wrap_text("short auuuuuuuuuuuuvery", 6),
            vec!["short", "auuuuu", "uuuuuu", "uvery"]
        );
        assert!(wrap_text("", 10).is_empty());
        // Newlines and runs of spaces collapse, like the JS split on whitespace.
        assert_eq!(wrap_text("a\n\nb   c", 20), vec!["a b c"]);
    }

    #[test]
    fn color_helpers_are_inert_when_color_is_off() {
        set_color(false);
        assert_eq!(dim("x"), "x");
        assert_eq!(bold("x"), "x");
        set_color(true);
        assert_eq!(bold("x"), "\x1b[1mx\x1b[0m");
        // The list view pads colored glyphs by exactly this much.
        assert_eq!(char_len(&green("●")) - char_len("●"), 9);
        set_color(false);
    }
}
