//! The interactive picker (standalone terminal only).
//!
//! Renders full-screen on the MAIN buffer, not the alternate screen (1049). iTerm2
//! suppresses any-motion mouse reporting while the alt screen is active until the
//! pointer re-enters the window, so hover felt dead when the cursor started inside;
//! the main buffer reports motion immediately. Rendering uses absolute positioning
//! (ESC[H + ESC[J), so we just hide the cursor and clear the screen ourselves.

use std::collections::HashMap;

use crate::model::{short_id, Node, Row};
use crate::render::{
    bold, char_len, clip, cyan, dim, format_time, green, pad_end, short_time, truncate, wrap_text,
    yellow,
};
use crate::search::filter_rows;
use crate::term;

pub struct Ctx {
    pub project_name: String,
    /// The current session belongs to another project than the one being shown.
    pub elsewhere: bool,
    pub current_id: Option<String>,
}

/// Append one rendered line: clear to end of line, then CRLF. Clearing per line
/// instead of clearing the screen is what keeps hover repaints flicker-free.
fn put(buf: &mut String, s: &str) {
    buf.push_str(s);
    buf.push_str("\x1b[K\r\n");
}

/// Expected byte length of a UTF-8 sequence from its lead byte.
fn utf8_len(b: u8) -> Option<usize> {
    if b < 0x80 {
        Some(1)
    } else if (0xc2..0xe0).contains(&b) {
        Some(2)
    } else if (0xe0..0xf0).contains(&b) {
        Some(3)
    } else if (0xf0..0xf5).contains(&b) {
        Some(4)
    } else {
        None
    }
}

/// `ESC [ < b ; x ; y (M|m)` — an SGR (1006) mouse report, or None for any other CSI.
fn parse_sgr_mouse(seq: &[u8]) -> Option<(u32, u32, u32, u8)> {
    if seq.len() < 4 || seq[0] != 0x1b || seq[1] != b'[' || seq[2] != b'<' {
        return None;
    }
    let kind = seq[seq.len() - 1];
    if kind != b'M' && kind != b'm' {
        return None;
    }
    let body = std::str::from_utf8(&seq[3..seq.len() - 1]).ok()?;
    let mut it = body.split(';');
    let b = it.next()?.parse::<u32>().ok()?;
    let x = it.next()?.parse::<u32>().ok()?;
    let y = it.next()?.parse::<u32>().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((b, x, y, kind))
}

struct Picker<'a> {
    nodes: &'a [Node],
    ctx: Ctx,
    /// The full tree, kept so a search can always be widened back out.
    all_rows: Vec<Row>,
    /// Current view: the whole tree, or the search-filtered subset.
    rows: Vec<Row>,
    /// Width of the index column, keyed to the FULL tree so it doesn't jitter as rows
    /// are filtered away while typing.
    idx_w: usize,
    selected: usize,
    scroll_top: usize,
    /// 1-based screen line -> row index, for mouse hit-testing.
    row_y_map: Vec<Option<usize>>,
    /// True while typing a query.
    search_mode: bool,
    query: String,
    /// Both maps hold ROW INDICES, so they are invalid the moment `rows` changes —
    /// `set_view` rebuilds them together with the view.
    row_by_session: HashMap<String, usize>,
    child_rows: HashMap<String, Vec<usize>>,
    /// Carries a partial escape sequence between reads.
    input_buf: Vec<u8>,
}

impl<'a> Picker<'a> {
    fn reindex(&mut self) {
        let mut by_id: HashMap<String, usize> = HashMap::with_capacity(self.rows.len());
        // Row indices of each node's children. `flatten` visits children in the mtime
        // order `build_forest` sorted them into, so the last entry is always the most
        // recent child.
        let mut child_rows: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, r) in self.rows.iter().enumerate() {
            let n = &self.nodes[r.node];
            by_id.insert(n.session_id.clone(), i);
            if let Some(p) = n.effective_parent.as_ref() {
                child_rows.entry(p.clone()).or_default().push(i);
            }
        }
        self.row_by_session = by_id;
        self.child_rows = child_rows;
    }

    /// Swap in a new view. The highlight stays put only if that branch is still a real
    /// match — otherwise it lands on the first hit. Falling back to row 0 instead put
    /// the selection on the topmost context ancestor, which reads as the search having
    /// picked the wrong branch.
    fn set_view(&mut self, next: Vec<Row>) {
        let keep_id: Option<String> = self
            .current()
            .map(|ri| self.nodes[self.rows[ri].node].session_id.clone());
        self.rows = next;
        self.reindex();
        let at = keep_id
            .as_ref()
            .and_then(|id| self.row_by_session.get(id).copied());
        match at {
            Some(i) if self.rows[i].matched != Some(false) => self.selected = i,
            _ => {
                self.selected = self
                    .rows
                    .iter()
                    .position(|r| r.matched != Some(false))
                    .unwrap_or(0);
            }
        }
        self.scroll_top = 0;
    }

    fn apply_query(&mut self, q: &str) {
        self.query = q.to_string();
        let next = filter_rows(self.nodes, &self.all_rows, &self.query);
        self.set_view(next);
        self.render();
    }

    /// `clear` also drops the filter; otherwise this just leaves typing mode.
    fn exit_search(&mut self, clear: bool) {
        self.search_mode = false;
        if clear && !self.query.is_empty() {
            self.apply_query("");
        } else {
            self.render();
        }
    }

    fn quit(&mut self, code: i32) -> ! {
        term::restore();
        std::process::exit(code);
    }

    fn resume(&mut self, node: usize) -> ! {
        term::restore();
        crate::launch_resume(&self.nodes[node].session_id);
    }

    /// A search can filter every row away, so nothing may read `rows[selected]` blindly.
    fn current(&self) -> Option<usize> {
        if self.rows.is_empty() || self.selected >= self.rows.len() {
            None
        } else {
            Some(self.selected)
        }
    }

    fn move_sel(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = (self.rows.len() - 1) as isize;
        let next = (self.selected as isize + delta).clamp(0, last);
        self.selected = next as usize;
        self.render();
    }

    fn jump_to(&mut self, i: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = (self.rows.len() - 1) as isize;
        self.selected = i.clamp(0, last) as usize;
        self.render();
    }

    /// Jump to the branch this one forked from. `effective_parent` is non-null only when
    /// that session is in this project, so it always resolves to a row; roots and forks
    /// whose parent lives elsewhere have nowhere to go.
    fn select_parent(&mut self) {
        let ri = match self.current() {
            Some(ri) => ri,
            None => return,
        };
        let pid = match self.nodes[self.rows[ri].node].effective_parent.clone() {
            Some(p) => p,
            None => return,
        };
        match self.row_by_session.get(&pid).copied() {
            Some(i) if i != self.selected => {
                self.selected = i;
                self.render();
            }
            _ => {}
        }
    }

    /// Descend to this branch's most recent child; leaves have nowhere to go.
    fn select_child(&mut self) {
        let ri = match self.current() {
            Some(ri) => ri,
            None => return,
        };
        let sid = self.nodes[self.rows[ri].node].session_id.clone();
        let last_child = match self.child_rows.get(&sid) {
            Some(kids) if !kids.is_empty() => kids[kids.len() - 1],
            _ => return,
        };
        self.selected = last_child;
        self.render();
    }

    fn row_text(&self, row_i: usize, cols: usize, is_sel: bool) -> String {
        let r = &self.rows[row_i];
        let n = &self.nodes[r.node];
        let idx = format!("{:>width$}", r.index, width = self.idx_w);
        let sid = short_id(&n.session_id);
        // Tag trailing the label: the current/most-recent marker when the row has one,
        // else a short last-active stamp so every row says when it was last touched.
        let stamp = short_time(n.mtime);
        let tag = if n.current {
            "  ← current".to_string()
        } else if n.latest {
            "  (most recent)".to_string()
        } else if !stamp.is_empty() {
            format!("  {}", stamp)
        } else {
            String::new()
        };
        let fixed = format!("{}  {}{}● {}  ", idx, r.prefix, r.connector, sid);
        let room = std::cmp::max(
            8,
            cols as isize - char_len(&fixed) as isize - char_len(&tag) as isize,
        ) as usize;
        let label = truncate(&n.label, room);
        let mut text = format!("{}{}{}", fixed, label, tag);
        if char_len(&text) > cols {
            text = clip(&text, cols);
        }
        if is_sel {
            return format!("\x1b[7m{}\x1b[0m", pad_end(&text, cols));
        }
        // A search context row — kept only to show where a match hangs off, not a hit
        // itself. Dim the whole line so the actual matches stand out; the usual per-part
        // coloring would just compete for attention.
        if r.matched == Some(false) {
            return dim(&text);
        }
        // Non-selected: colorize glyph + id; bold a title/name, plain a first prompt.
        let glyph = if n.current {
            green("●")
        } else if n.latest {
            yellow("●")
        } else {
            "●".to_string()
        };
        let shown_label = if n.strong { bold(&label) } else { label };
        format!(
            "{}  {}{}{} {}  {}{}",
            idx,
            r.prefix,
            r.connector,
            glyph,
            cyan(sid),
            shown_label,
            if tag.is_empty() {
                String::new()
            } else {
                dim(&tag)
            }
        )
    }

    fn build_detail(&self, node: usize, cols: usize) -> Vec<String> {
        let n = &self.nodes[node];
        let width = std::cmp::min(cols, 100);
        let mut lines: Vec<String> = Vec::new();
        lines.push(dim(&"─".repeat(std::cmp::min(cols, 80))));
        let parent = match n.effective_parent.as_deref() {
            Some(p) => {
                let mut s = format!("forked from {}", short_id(p));
                if let Some(f) = n.fork_msg.as_deref() {
                    s.push_str(&format!(" @ {}", short_id(f)));
                }
                s
            }
            None => "root session".to_string(),
        };
        lines.push(bold(&n.session_id));
        lines.push(dim(&format!(
            "{} · last active {}",
            parent,
            format_time(n.mtime)
        )));
        lines.push(String::new());
        // Heading: the winning name/title computed in scan_session (a rename supersedes
        // the generated title rather than sitting alongside it). Body is always the
        // first prompt, so a titled/named branch shows both.
        if let Some(h) = n.heading.as_deref() {
            lines.push(bold(&truncate(h, width)));
        }
        let body = if n.prompt_full.is_empty() {
            dim("(no prompt text)")
        } else {
            n.prompt_full.clone()
        };
        let wrapped = wrap_text(&body, width);
        let max_lines = 6;
        for wl in wrapped.iter().take(max_lines) {
            lines.push(wl.clone());
        }
        if wrapped.len() > max_lines {
            lines.push(dim("…"));
        }
        lines
    }

    fn render(&mut self) {
        let (cols, term_rows) = term::term_size();
        // With no matches there is no branch to describe, so the detail panel collapses
        // to a single hint line rather than reading rows[selected] off an empty view.
        let detail: Vec<String> = match self.current() {
            Some(ri) => self.build_detail(self.rows[ri].node, cols),
            None => vec![
                dim(&"─".repeat(std::cmp::min(cols, 80))),
                dim("no branch matches this search"),
            ],
        };
        // Shown between header and rows.
        let search_line = self.search_mode || !self.query.is_empty();
        // header + [search] + blank
        let chrome_top = 2 + if search_line { 1 } else { 0 };
        // blank + detail + blank + footer
        let chrome_bottom = 1 + detail.len() + 1 + 1;
        let mut viewport = term_rows as isize - chrome_top as isize - chrome_bottom as isize;
        if viewport < 3 {
            viewport = 3;
        }
        let viewport = viewport as usize;
        if self.selected < self.scroll_top {
            self.scroll_top = self.selected;
        }
        if self.selected >= self.scroll_top + viewport {
            self.scroll_top = self.selected + 1 - viewport;
        }
        let end = std::cmp::min(self.rows.len(), self.scroll_top + viewport);

        // Build the whole frame, then write once.
        let mut buf = String::with_capacity(8192);
        buf.push_str("\x1b[H");
        let mut header = bold(&format!("Branches in {}", self.ctx.project_name));
        if self.rows.len() > viewport {
            header.push_str(&dim(&format!(
                "  ({}/{})",
                self.selected + 1,
                self.rows.len()
            )));
        }
        put(&mut buf, &header);
        if search_line {
            // Count hits, not rows: the view also carries dimmed context ancestors, and
            // reporting those as matches overstates what was found.
            let hits = self
                .rows
                .iter()
                .filter(|r| r.matched != Some(false))
                .count();
            let count = if self.query.is_empty() {
                String::new()
            } else if hits == 0 {
                "  no matches".to_string()
            } else {
                format!("  {} of {}", hits, self.all_rows.len())
            };
            // A block cursor while typing, so it's obvious the keyboard is going here.
            let cursor = if self.search_mode {
                "\x1b[7m \x1b[0m"
            } else {
                ""
            };
            let line = format!(
                "{}{}{}{}",
                cyan("search: "),
                self.query,
                cursor,
                dim(&count)
            );
            put(&mut buf, &line);
        }
        put(&mut buf, "");
        self.row_y_map.clear();
        self.row_y_map.resize(term_rows + 3, None);
        for (offset, i) in (self.scroll_top..end).enumerate() {
            let y = chrome_top + 1 + offset;
            if y < self.row_y_map.len() {
                self.row_y_map[y] = Some(i);
            }
            let text = self.row_text(i, cols, i == self.selected);
            put(&mut buf, &text);
        }
        for _ in end..(self.scroll_top + viewport) {
            put(&mut buf, "");
        }
        put(&mut buf, "");
        for dl in detail.iter() {
            put(&mut buf, dl);
        }
        put(&mut buf, "");
        if self.ctx.elsewhere {
            if let Some(id) = self.ctx.current_id.as_deref() {
                let line = dim(&format!(
                    "current session {} is in another project",
                    short_id(id)
                ));
                put(&mut buf, &line);
            }
        }
        let footer = if self.search_mode {
            "type to filter   ↑/↓: select   Enter: accept   Esc: cancel"
        } else if !self.query.is_empty() {
            "↑/↓/hover: navigate   p/←: parent   →: child   Enter/click: resume   Esc: clear search"
        } else {
            "↑/↓/hover: navigate   s: search   p/←: parent   →: child   Enter/click: resume   Esc: quit"
        };
        buf.push_str(&dim(footer));
        buf.push_str("\x1b[K");
        // Clear anything left below from a previous taller frame.
        buf.push_str("\x1b[J");
        term::write_out(&buf);
    }

    fn handle_mouse(&mut self, b: u32, yy: u32, kind: u8) {
        // Ignore the wheel — scrolling must not change selection.
        if b == 64 || b == 65 {
            return;
        }
        let row = match self.row_y_map.get(yy as usize).copied().flatten() {
            Some(r) => r,
            // Not over a branch row.
            None => return,
        };
        let is_motion = (b & 32) != 0;
        if is_motion {
            // Hover -> select the row under the cursor.
            if row != self.selected {
                self.selected = row;
                self.render();
            }
            return;
        }
        // Left-click -> resume that branch.
        if kind == b'M' && (b & 3) == 0 {
            self.selected = row;
            let node = self.rows[row].node;
            self.resume(node);
        }
    }

    /// A lone ESC that never grew into a sequence within the timeout = the Escape key.
    /// Escape unwinds one layer at a time: typing -> filter -> quit. Only a bare picker
    /// with no search in play exits, so a search can never be lost to a stray keypress.
    fn flush_pending(&mut self) {
        let pending = std::mem::take(&mut self.input_buf);
        // Stalled/garbled partial sequence: drop it.
        if pending.len() != 1 || pending[0] != 0x1b {
            return;
        }
        if self.search_mode {
            self.exit_search(true);
        } else if !self.query.is_empty() {
            self.apply_query("");
        } else {
            self.quit(0);
        }
    }

    fn dispatch_csi_final(&mut self, f: u8) {
        match f {
            b'A' => self.move_sel(-1),
            b'B' => self.move_sel(1),
            b'H' => self.jump_to(0),
            b'F' => {
                let last = self.rows.len() as isize - 1;
                self.jump_to(last);
            }
            b'D' => self.select_parent(),
            b'C' => self.select_child(),
            // Any other CSI (focus events, etc.): ignore rather than quit.
            _ => {}
        }
    }

    /// Terminal input can split an escape sequence across reads — in any-motion mouse
    /// mode (1003), motion floods events, so mid-sequence splits are common. A naive
    /// parser mis-handles a split: it advances past the stray ESC and desyncs the
    /// stream, so hover/clicks do nothing until the next event happens to resync (and a
    /// split right at the ESC byte could even quit). So we buffer a partial sequence and
    /// re-join it with the next chunk, and parse CSI/SS3 generically — unknown or
    /// incomplete sequences are never mistaken for the Escape key.
    fn on_data(&mut self, data: &[u8]) {
        let mut buf: Vec<u8> = Vec::with_capacity(self.input_buf.len() + data.len());
        buf.extend_from_slice(&self.input_buf);
        buf.extend_from_slice(data);
        self.input_buf.clear();
        let mut i = 0usize;
        while i < buf.len() {
            let ch = buf[i];
            if ch != 0x1b {
                // Search mode swallows every printable key, so j/k/p/q/g type instead of
                // navigating. Arrows (handled below) still move the selection.
                if self.search_mode {
                    if ch == 0x03 {
                        // Ctrl-C always quits.
                        self.quit(0);
                    } else if ch == b'\r' || ch == b'\n' {
                        // Accept, keeping the filter — but never onto an empty view,
                        // which would strand the picker with nothing to select.
                        if !self.rows.is_empty() {
                            self.exit_search(false);
                        }
                    } else if ch == 0x7f || ch == 0x08 {
                        let mut q = self.query.clone();
                        q.pop();
                        self.apply_query(&q);
                    } else if ch == 0x15 {
                        // Ctrl-U: clear query.
                        self.apply_query("");
                    } else if ch >= 0x20 {
                        match utf8_len(ch) {
                            // Stray continuation byte: drop it.
                            None => {
                                i += 1;
                                continue;
                            }
                            Some(len) => {
                                if i + len > buf.len() {
                                    // Multi-byte char split across reads: await the rest.
                                    self.input_buf = buf[i..].to_vec();
                                    return;
                                }
                                if let Ok(s) = std::str::from_utf8(&buf[i..i + len]) {
                                    let q = format!("{}{}", self.query, s);
                                    self.apply_query(&q);
                                }
                                i += len;
                                continue;
                            }
                        }
                    }
                    i += 1;
                    continue;
                }
                if ch == b'\r' || ch == b'\n' {
                    if let Some(ri) = self.current() {
                        let node = self.rows[ri].node;
                        self.resume(node);
                    }
                } else if ch == b's' || ch == b'/' {
                    self.search_mode = true;
                    self.render();
                } else if ch == b'k' {
                    self.move_sel(-1);
                } else if ch == b'j' {
                    self.move_sel(1);
                } else if ch == b'p' {
                    self.select_parent();
                } else if ch == b'g' {
                    self.jump_to(0);
                } else if ch == b'G' {
                    let last = self.rows.len() as isize - 1;
                    self.jump_to(last);
                } else if ch == b'q' || ch == 0x03 {
                    self.quit(0);
                }
                i += 1;
                continue;
            }
            let rest = &buf[i..];
            // ESC key, or the start of a sequence still in flight.
            if rest.len() == 1 {
                self.input_buf = vec![0x1b];
                return;
            }
            if rest[1] == b'[' {
                // CSI: params [0x30-0x3f], intermediates [0x20-0x2f], final [0x40-0x7e].
                let mut j = 2usize;
                while j < rest.len() && (0x30..=0x3f).contains(&rest[j]) {
                    j += 1;
                }
                while j < rest.len() && (0x20..=0x2f).contains(&rest[j]) {
                    j += 1;
                }
                if j >= rest.len() || !(0x40..=0x7e).contains(&rest[j]) {
                    // Incomplete -> await more bytes.
                    self.input_buf = rest.to_vec();
                    return;
                }
                let seq_len = j + 1;
                let seq = rest[..seq_len].to_vec();
                match parse_sgr_mouse(&seq) {
                    Some((b, _x, yy, kind)) => self.handle_mouse(b, yy, kind),
                    None => self.dispatch_csi_final(seq[seq_len - 1]),
                }
                i += seq_len;
                continue;
            }
            if rest[1] == b'O' {
                // SS3 application cursor keys: ESC O <final>.
                if rest.len() < 3 {
                    self.input_buf = rest.to_vec();
                    return;
                }
                let f = rest[2];
                self.dispatch_csi_final(f);
                i += 3;
                continue;
            }
            // ESC + other byte (Alt-combo, etc.). Mid-typing that must not kill the
            // picker and lose the query, so swallow it; outside search mode it quits.
            if self.search_mode {
                i += 2;
                continue;
            }
            self.quit(0);
        }
    }
}

/// Exit code for a signal that reached us from outside.
fn signal_exit_code(sig: i32) -> i32 {
    match sig {
        libc::SIGINT => 130,
        libc::SIGTERM => 143,
        _ => 129,
    }
}

/// Run the picker. Hands the terminal to `claude -r` on Enter/click, or exits on quit;
/// either way it never returns.
pub fn run(nodes: &[Node], all_rows: Vec<Row>, ctx: Ctx) -> ! {
    let idx_w = all_rows.len().to_string().len();
    let mut p = Picker {
        nodes,
        ctx,
        rows: all_rows.clone(),
        all_rows,
        idx_w,
        selected: 0,
        scroll_top: 0,
        row_y_map: Vec::new(),
        search_mode: false,
        query: String::new(),
        row_by_session: HashMap::new(),
        child_rows: HashMap::new(),
        input_buf: Vec::new(),
    };
    p.reindex();
    p.selected = p
        .rows
        .iter()
        .position(|r| nodes[r.node].current || nodes[r.node].latest)
        .unwrap_or(0);

    term::install_signal_handlers();
    if !term::set_raw() {
        eprintln!("branch-graph: could not put the terminal in raw mode");
        std::process::exit(1);
    }
    // Hide cursor, then enable any-motion mouse tracking (1003 also reports button
    // press/release) with SGR coordinates (1006). We deliberately do NOT set 1000 as
    // well: 1000 and 1003 are alternate tracking modes and setting 1000 first can leave
    // terminals in press-only state.
    term::write_out("\x1b[?25l");
    term::write_out("\x1b[?1003h\x1b[?1006h");
    // Clear the visible screen, cursor home.
    term::write_out("\x1b[2J\x1b[H");
    p.render();

    let mut last_size = term::term_size();
    let mut buf = [0u8; 4096];
    loop {
        let sig = term::pending_signal();
        if sig != 0 {
            p.quit(signal_exit_code(sig));
        }
        // Node had a `resize` event for this; polling the size on each tick is the same
        // information without a signal handler in the middle of a render.
        let size = term::term_size();
        if size != last_size {
            last_size = size;
            p.render();
        }
        // A pending partial sequence is what the 50ms window is for: if nothing arrives
        // to complete it, a lone ESC was the Escape key. Otherwise just idle.
        let timeout = if p.input_buf.is_empty() { 200 } else { 50 };
        match term::poll_stdin(timeout) {
            // Interrupted by a signal: re-check state and loop.
            Err(()) => continue,
            Ok(false) => {
                if !p.input_buf.is_empty() {
                    p.flush_pending();
                }
            }
            Ok(true) => {
                let n = term::read_stdin(&mut buf);
                if n == 0 {
                    p.quit(0);
                }
                if n < 0 {
                    continue;
                }
                p.on_data(&buf[..n as usize]);
            }
        }
    }
}

// ---------- mouse diagnostic ----------

fn debug_paint(count: u64, last: &str) {
    let mut b = String::from("\x1b[H\x1b[2J");
    b.push_str("Mouse debug — alt-screen, mirrors the picker.\r\n");
    b.push_str("MOVE inside WITHOUT clicking or re-entering the window. Press q to quit.\r\n\r\n");
    b.push_str(&format!("motion events: {}\r\n", count));
    b.push_str(&format!("last: {}\r\n", last));
    if count == 0 {
        b.push_str(
            "\r\n(still 0 — keep moving inside. If it only counts up after you move out\r\n",
        );
        b.push_str(" and back in, that confirms the alt-screen motion-onset quirk.)\r\n");
    }
    term::write_out(&b);
}

fn debug_finish(count: u64, last: &str) -> ! {
    // Mouse off first, then `restore` returns the terminal and clears the alt screen,
    // then leave it — so the summary lands on the user's untouched main screen.
    term::write_out("\x1b[?1003l\x1b[?1006l");
    term::restore();
    term::write_out("\x1b[?1049l");
    println!(
        "\nSaw {} motion event(s) on the alternate screen. last: {}",
        count, last
    );
    println!(
        "{}",
        if count == 0 {
            "iTerm2 quirk CONFIRMED: motion is suppressed on the alt screen until re-entry."
        } else {
            "Alt screen reports motion fine — the picker bug is elsewhere."
        }
    );
    std::process::exit(0);
}

/// `branch-graph --debug-mouse`: mirrors the picker's setup EXACTLY (alternate screen
/// with any-motion tracking) and counts motion events. Move the pointer inside WITHOUT
/// clicking or re-entering the window. On exit it reports how many motion events it
/// saw — if 0 until you move out and back in, the terminal suppresses motion on the
/// alternate screen (the iTerm2 quirk this chases); if it counts up immediately, the
/// terminal is fine and the bug is elsewhere.
pub fn debug_mouse() -> ! {
    if !term::isatty(1) || !term::isatty(0) {
        eprintln!(
            "branch-graph --debug-mouse: needs a real terminal (run it directly in your shell, not via Claude Code `!`)."
        );
        std::process::exit(2);
    }
    term::install_signal_handlers();
    if !term::set_raw() {
        eprintln!("branch-graph: could not put the terminal in raw mode");
        std::process::exit(1);
    }
    // Same init order/sequences as the picker, but on the alternate screen.
    term::write_out("\x1b[?1049h\x1b[?25l");
    term::write_out("\x1b[?1003h\x1b[?1006h");
    let mut count: u64 = 0;
    let mut last = String::from("none");
    debug_paint(count, &last);
    let mut buf = [0u8; 4096];
    loop {
        if term::pending_signal() != 0 {
            debug_finish(count, &last);
        }
        match term::poll_stdin(200) {
            Err(()) => continue,
            Ok(false) => continue,
            Ok(true) => {}
        }
        let n = term::read_stdin(&mut buf);
        if n <= 0 {
            debug_finish(count, &last);
        }
        let data = &buf[..n as usize];
        if data.contains(&b'q') || data.contains(&0x03) {
            debug_finish(count, &last);
        }
        // First mouse report in the chunk, like the single regex exec in the original.
        let mut k = 0usize;
        while k + 3 < data.len() {
            if data[k] == 0x1b && data[k + 1] == b'[' && data[k + 2] == b'<' {
                let mut j = k + 3;
                while j < data.len() && data[j] != b'M' && data[j] != b'm' {
                    j += 1;
                }
                if j < data.len() {
                    if let Some((b, x, y, kind)) = parse_sgr_mouse(&data[k..=j]) {
                        if b & 32 != 0 {
                            count += 1;
                            last = format!("b={} x={} y={} {}", b, x, y, kind as char);
                            debug_paint(count, &last);
                        }
                    }
                }
                break;
            }
            k += 1;
        }
    }
}
