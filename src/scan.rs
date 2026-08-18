//! Reading one session transcript and deciding what the branch is called.
//!
//! A `/branch` fork replays the parent's history into the new file, tagging every
//! copied line with `forkedFrom` (whose `messageUuid` equals that line's own uuid).
//! The branch's OWN messages have no `forkedFrom`.
//!
//! Within a single session, rewinding to an earlier prompt and retyping appends a
//! NEW sibling: the old turns stay earlier in the file but become orphaned, and the
//! new turns hang off the same parent. So we must not pick prompts by file order —
//! we follow the ACTIVE path (current leaf back to root) and read the first own
//! prompt along it. Claude Code records the live head as the latest `last-prompt`
//! line's `leafUuid`; we fall back to the last message in the file.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::model::{short_id, Node};

/// How many non-empty lines `first_fork_parent` reads before giving up. A fork's
/// replayed history starts at line 1, so 1 would do; the slack costs nothing and
/// absorbs a stray leading record without falling back to a full scan.
const FORK_PROBE_LINES: usize = 5;

// ---------- line records ----------
//
// Only the fields this tool reads are declared. Everything else in a line —
// including the large `message` bodies and tool results — is skipped by serde
// without being allocated, which is what makes the scan fast.

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ForkedFrom {
    session_id: Option<String>,
    message_uuid: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(rename = "type")]
    kind: Option<String>,
    uuid: Option<String>,
    parent_uuid: Option<String>,
    is_meta: Option<bool>,
    leaf_uuid: Option<String>,
    forked_from: Option<ForkedFrom>,
    ai_title: Option<String>,
    custom_title: Option<String>,
    slug: Option<String>,
    title: Option<String>,
    content: Option<String>,
}

/// A user line's typed text. Parsed separately, and only for `type: "user"` lines,
/// so assistant/tool payloads are never materialized. `message.content` is an array
/// for tool results; that fails to deserialize into `String`, which is exactly the
/// `typeof content === 'string'` check the JS version makes.
#[derive(Deserialize)]
struct UserLine {
    message: Option<UserMessage>,
}

#[derive(Deserialize)]
struct UserMessage {
    content: Option<String>,
}

/// Field-by-field fallback for a line the narrow struct rejects (a field carrying
/// an unexpected type). Mirrors the JS `typeof x === 'string'` checks, so one odd
/// line degrades to "those fields are absent" instead of dropping the line — and
/// with it that line's place in the parent chain.
fn record_from_value(v: &Value) -> Record {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    Record {
        kind: s("type"),
        uuid: s("uuid"),
        parent_uuid: s("parentUuid"),
        is_meta: v.get("isMeta").and_then(|x| x.as_bool()),
        leaf_uuid: s("leafUuid"),
        forked_from: v
            .get("forkedFrom")
            .filter(|f| !f.is_null())
            .map(|f| ForkedFrom {
                session_id: f
                    .get("sessionId")
                    .and_then(|x| x.as_str())
                    .map(String::from),
                message_uuid: f
                    .get("messageUuid")
                    .and_then(|x| x.as_str())
                    .map(String::from),
            }),
        ai_title: s("aiTitle"),
        custom_title: s("customTitle"),
        slug: s("slug"),
        title: s("title"),
        content: s("content"),
    }
}

// ---------- label cleanup ----------

/// Drop SGR sequences (`ESC [ ... m`), e.g. from pasted tool output.
fn strip_ansi(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match rest.find('\x1b') {
            None => {
                out.push_str(rest);
                return out;
            }
            Some(i) => {
                out.push_str(&rest[..i]);
                let after = &rest[i..];
                let b = after.as_bytes();
                let mut j = 2;
                if b.len() > 1 && b[1] == b'[' {
                    while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';') {
                        j += 1;
                    }
                    if j < b.len() && b[j] == b'm' {
                        rest = &after[j + 1..];
                        continue;
                    }
                }
                out.push('\x1b');
                rest = &after[1..];
            }
        }
    }
}

/// Remove `<{prefix}…>…</{prefix}…>` spans, shortest match first — the slash-command
/// wrappers Claude Code stores around a typed `/command`.
fn strip_tag_block(s: &str, prefix: &str) -> String {
    let open = format!("<{}", prefix);
    let close = format!("</{}", prefix);
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let i = match rest.find(&open) {
            Some(i) => i,
            None => {
                out.push_str(rest);
                return out;
            }
        };
        // The open tag ends at its first '>' (the regex body is `[^>]*`).
        let after_open = &rest[i + open.len()..];
        let gt = match after_open.find('>') {
            Some(g) => g,
            None => {
                out.push_str(rest);
                return out;
            }
        };
        let body_start = i + open.len() + gt + 1;
        let tail = &rest[body_start..];
        let ci = match tail.find(&close) {
            Some(c) => c,
            None => {
                out.push_str(rest);
                return out;
            }
        };
        let after_close = &tail[ci + close.len()..];
        let cgt = match after_close.find('>') {
            Some(g) => g,
            None => {
                out.push_str(rest);
                return out;
            }
        };
        out.push_str(&rest[..i]);
        rest = &rest[body_start + ci + close.len() + cgt + 1..];
    }
}

/// Remove any remaining `<…>` tag (at least one char between the brackets).
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let i = match rest.find('<') {
            Some(i) => i,
            None => {
                out.push_str(rest);
                return out;
            }
        };
        let after = &rest[i + 1..];
        match after.find('>') {
            Some(g) if g >= 1 => {
                out.push_str(&rest[..i]);
                rest = &after[g + 1..];
            }
            _ => {
                out.push_str(&rest[..i + 1]);
                rest = after;
            }
        }
    }
}

/// Squeeze a stored prompt into one clean line fit for a label.
pub fn clean_prompt(s: &str) -> String {
    let s = strip_ansi(s);
    let s = strip_tag_block(&s, "command-");
    let s = strip_tag_block(&s, "local-command-");
    let s = strip_tags(&s);
    s.split_whitespace().collect::<Vec<&str>>().join(" ")
}

/// Claude Code auto-names a `/branch` (created without a name) as "<…> (Branch N)".
/// We only treat an EXPLICIT `/branch myname` name as a real name, so auto names are
/// ignored and those branches fall back to their first prompt.
fn is_auto_branch_name(s: &str) -> bool {
    let t = s.trim_end();
    if !t.ends_with(')') {
        return false;
    }
    let inner_end = t.len() - 1;
    let open = match t[..inner_end].rfind('(') {
        Some(i) => i,
        None => return false,
    };
    let inner = &t[open + 1..inner_end];
    if inner == "Branch" {
        return true;
    }
    match inner.strip_prefix("Branch") {
        // `\(Branch(\s+\d+)?\)`: whitespace then digits, nothing else.
        Some(rest) => {
            let digits = rest.trim_start();
            digits.len() < rest.len()
                && !digits.is_empty()
                && digits.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Keep at most `n` chars of `s`.
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---------- scanning ----------

/// One line of the message graph, as far as this tool cares.
struct LineNode {
    parent: Option<String>,
    is_user: bool,
    is_meta: bool,
    fork: bool,
    /// Typed text, kept only for the user lines that can supply a first prompt.
    content: Option<String>,
}

/// Scan one transcript into a `Node`.
///
/// An unreadable or truncated transcript yields a node labelled with its short id
/// rather than failing the whole run: a session being written to right now is a
/// normal thing to encounter, and dropping the file would also drop every branch
/// forked from it.
pub fn scan_session(file: &Path, session_id: &str) -> Node {
    let mut info = Node::new(session_id.to_string());

    let mut title: Option<String> = None;
    let mut slug: Option<String> = None;
    let mut name: Option<String> = None;
    // Line numbers of the last title/name write, so we can tell which one is newer.
    let mut title_at: i64 = -1;
    let mut name_at: i64 = -1;
    let mut line_no: i64 = 0;
    // messageUuid of the last replayed line = divergence leaf.
    let mut last_fork_msg: Option<String> = None;
    let mut leaf_uuid: Option<String> = None;
    let mut last_uuid: Option<String> = None;
    let mut nodes: HashMap<String, LineNode> = HashMap::new();

    let handle = match File::open(file) {
        Ok(f) => f,
        Err(_) => {
            info.label = short_id(session_id).to_string();
            return info;
        }
    };
    let mut reader = BufReader::with_capacity(1 << 16, handle);
    let mut raw: Vec<u8> = Vec::with_capacity(4096);
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        while raw.last() == Some(&b'\n') || raw.last() == Some(&b'\r') {
            raw.pop();
        }
        line_no += 1;
        if raw.is_empty() {
            continue;
        }
        let line = String::from_utf8_lossy(&raw);
        let rec: Record = match serde_json::from_str::<Record>(&line) {
            Ok(r) => r,
            Err(_) => match serde_json::from_str::<Value>(&line) {
                Ok(v) => record_from_value(&v),
                Err(_) => continue,
            },
        };

        let kind = rec.kind.as_deref().unwrap_or("");

        // Live head pointer.
        if kind == "last-prompt" {
            if let Some(l) = rec.leaf_uuid.as_ref().filter(|s| !s.is_empty()) {
                leaf_uuid = Some(l.clone());
            }
        }
        let is_fork_line = rec.forked_from.is_some();
        if let Some(f) = rec.forked_from.as_ref() {
            if let Some(sid) = f.session_id.as_ref().filter(|s| !s.is_empty()) {
                if info.parent.is_none() {
                    info.parent = Some(sid.clone());
                }
                if let Some(mu) = f.message_uuid.as_ref().filter(|s| !s.is_empty()) {
                    last_fork_msg = Some(mu.clone());
                }
            }
        }
        // Claude's own summary. NOTE: `/rename` on the LIVE session is also recorded
        // here (it overwrites aiTitle), so this field is not purely machine-generated.
        match rec
            .ai_title
            .as_ref()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
        {
            Some(t) => {
                title = Some(t.to_string());
                title_at = line_no;
            }
            None => {
                if kind == "ai-title" {
                    let t = rec
                        .title
                        .as_ref()
                        .or(rec.ai_title.as_ref())
                        .or(rec.content.as_ref())
                        .map(|t| t.trim())
                        .filter(|t| !t.is_empty());
                    if let Some(t) = t {
                        title = Some(t.to_string());
                        title_at = line_no;
                    }
                }
            }
        }
        // User-set display name (the one shown in /resume), from `/rename` or
        // `/branch <name>`. Last EXPLICIT write wins: auto "<parent> (Branch N)" names
        // are skipped here so a later auto write can't clobber a name the user typed.
        if let Some(t) = rec
            .custom_title
            .as_ref()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
        {
            if !is_auto_branch_name(t) {
                name = Some(t.to_string());
                name_at = line_no;
            }
        }
        if slug.is_none() {
            // An empty slug is falsy in the JS original, so it must neither become a
            // label nor block a later real one.
            if let Some(s) = rec.slug.as_ref().filter(|s| !s.is_empty()) {
                slug = Some(s.clone());
            }
        }
        if let Some(uuid) = rec.uuid.as_ref().filter(|s| !s.is_empty()) {
            last_uuid = Some(uuid.clone());
            let is_meta = rec.is_meta.unwrap_or(false);
            let is_user = kind == "user";
            let content = if is_user && !is_meta {
                serde_json::from_str::<UserLine>(&line)
                    .ok()
                    .and_then(|u| u.message)
                    .and_then(|m| m.content)
                    .map(|c| take_chars(&c, 4096))
            } else {
                None
            };
            nodes.insert(
                uuid.clone(),
                LineNode {
                    parent: rec.parent_uuid.filter(|s| !s.is_empty()),
                    is_user,
                    is_meta,
                    fork: is_fork_line,
                    content,
                },
            );
        }
    }

    // Walk the active path: current leaf -> root, then read it root-first.
    let head = match leaf_uuid {
        Some(ref l) if nodes.contains_key(l) => Some(l.clone()),
        _ => last_uuid,
    };
    let mut path: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cur = head;
    while let Some(id) = cur {
        if !nodes.contains_key(&id) || seen.contains(&id) {
            break;
        }
        let next = nodes.get(&id).and_then(|n| n.parent.clone());
        seen.insert(id.clone());
        path.push(id);
        cur = next;
    }
    path.reverse();

    // First own (non-replayed) node on the active path marks the divergence; the first
    // own user message with text is this branch's first typed prompt.
    let mut first_prompt: Option<String> = None;
    let mut first_new_parent: Option<String> = None;
    let mut saw_new = false;
    for u in &path {
        let n = match nodes.get(u) {
            Some(n) => n,
            None => continue,
        };
        if n.fork {
            continue;
        }
        if !saw_new {
            saw_new = true;
            first_new_parent = n.parent.clone();
        }
        if first_prompt.is_none() && n.is_user && !n.is_meta {
            if let Some(c) = n.content.as_ref() {
                let cleaned = clean_prompt(c);
                if !cleaned.is_empty() {
                    first_prompt = Some(cleaned);
                }
            }
        }
    }

    info.title = title.clone();
    // `name` already excludes auto "(Branch N)" names (filtered while scanning).
    info.name = name
        .as_ref()
        .map(|n| clean_prompt(n))
        .filter(|n| !n.is_empty());
    info.named = info.name.is_some();
    // Fork point: prefer the divergence message in the parent (first own node's parent);
    // fall back to the last replayed message when the fork has no new turns.
    info.fork_msg = if info.parent.is_some() {
        first_new_parent.or(last_fork_msg)
    } else {
        None
    };
    // A session can be named from two places: a `custom-title` record (`/rename` on a
    // non-live session, `/branch <name>`) or an `aiTitle` write (Claude's summary, and
    // `/rename` on the live session). Neither kind outranks the other — whichever landed
    // LAST in the transcript is what Claude Code itself displays, so a rename always
    // supersedes an older title and vice versa. Falls back to the first typed prompt.
    info.heading = match info.name.clone() {
        Some(n) if name_at >= title_at => Some(n),
        other => title.clone().or(other),
    };
    info.label = info
        .heading
        .clone()
        .or_else(|| first_prompt.clone())
        .or_else(|| slug.clone())
        .unwrap_or_else(|| short_id(session_id).to_string());
    // Bold for a name/title, plain for a first prompt.
    info.strong = info.heading.is_some();
    info.prompt_full = take_chars(&first_prompt.or(slug).unwrap_or_default(), 4000);
    info
}

/// The sessionId `file` forked from, read from the head of the transcript, or None if
/// the first few lines carry no `forkedFrom`.
///
/// This is what makes `..` cheap. `scan_session` takes the FIRST `forkedFrom` it sees,
/// and a fork tags every replayed line — starting at line 1 — so the answer is in the
/// first line of the file. Parsing every transcript in the project to learn it costs
/// real time on a large project; reading one line costs microseconds.
///
/// Callers must read None as "don't know", NOT "root": a root session and a fork whose
/// replay somehow starts past the probe window look identical from here, and only a full
/// scan can tell them apart (and only it has the label the root diagnostic prints).
pub fn first_fork_parent(file: &Path) -> Option<String> {
    let handle = File::open(file).ok()?;
    let mut reader = BufReader::with_capacity(1 << 16, handle);
    let mut raw: Vec<u8> = Vec::with_capacity(4096);
    let mut seen = 0usize;
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) => return None,
            Ok(_) => {}
            Err(_) => return None,
        }
        while raw.last() == Some(&b'\n') || raw.last() == Some(&b'\r') {
            raw.pop();
        }
        if raw.is_empty() {
            continue;
        }
        let line = String::from_utf8_lossy(&raw);
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            let from = v
                .get("forkedFrom")
                .and_then(|f| f.get("sessionId"))
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty());
            if let Some(from) = from {
                return Some(from.to_string());
            }
        }
        seen += 1;
        if seen >= FORK_PROBE_LINES {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);
    const SID: &str = "cccccccc-0000-0000-0000-000000000000";

    fn scan_lines(lines: &[&str]) -> Node {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("bg-scan-test-{}-{}.jsonl", std::process::id(), n));
        std::fs::write(&path, lines.join("\n")).unwrap();
        let node = scan_session(&path, SID);
        let _ = std::fs::remove_file(&path);
        node
    }

    #[test]
    fn clean_prompt_strips_ansi_command_wrappers_and_tags() {
        assert_eq!(clean_prompt("\x1b[32mgreen\x1b[0m text"), "green text");
        assert_eq!(
            clean_prompt("<command-name>/branch</command-name>keep this"),
            "keep this"
        );
        assert_eq!(
            clean_prompt("<local-command-stdout>noise</local-command-stdout> real"),
            "real"
        );
        assert_eq!(clean_prompt("a <b>c</b>  d\n\ne"), "a c d e");
        // A bare `<` is not a tag and must survive.
        assert_eq!(clean_prompt("if a < b then"), "if a < b then");
    }

    #[test]
    fn auto_branch_names_are_not_real_names() {
        assert!(is_auto_branch_name("Fix login (Branch 2)"));
        assert!(is_auto_branch_name("Fix login (Branch)"));
        assert!(is_auto_branch_name("Fix login (Branch 12)   "));
        assert!(!is_auto_branch_name("stripe-webhooks"));
        assert!(!is_auto_branch_name("Branch 2"));
        assert!(!is_auto_branch_name("(Branch two)"));
        assert!(!is_auto_branch_name("(Branchy)"));
    }

    /// The reason prompts are read along the active path rather than in file order: a
    /// rewind leaves the abandoned turns earlier in the file, and `last-prompt` names
    /// the live head.
    #[test]
    fn first_prompt_follows_the_active_path_not_file_order() {
        let node = scan_lines(&[
            r#"{"type":"user","uuid":"u1","parentUuid":null,"forkedFrom":{"sessionId":"parent-1","messageUuid":"u1"},"message":{"content":"replayed parent prompt"}}"#,
            r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","forkedFrom":{"sessionId":"parent-1","messageUuid":"a1"}}"#,
            r#"{"type":"user","uuid":"u2","parentUuid":"a1","message":{"content":"abandoned rewind prompt"}}"#,
            r#"{"type":"assistant","uuid":"a2","parentUuid":"u2"}"#,
            r#"{"type":"user","uuid":"u3","parentUuid":"a1","message":{"content":"retyped prompt after rewind"}}"#,
            r#"{"type":"assistant","uuid":"a3","parentUuid":"u3"}"#,
            r#"{"type":"last-prompt","leafUuid":"a3"}"#,
        ]);
        assert_eq!(node.parent.as_deref(), Some("parent-1"));
        // Divergence point is the parent of the first own turn on the active path.
        assert_eq!(node.fork_msg.as_deref(), Some("a1"));
        assert_eq!(node.prompt_full, "retyped prompt after rewind");
        assert_eq!(node.label, "retyped prompt after rewind");
        // A first prompt is plain text, not a bold heading.
        assert!(!node.strong);
        assert!(node.heading.is_none());
    }

    #[test]
    fn last_write_wins_between_a_rename_and_a_generated_title() {
        let name_last = scan_lines(&[
            r#"{"type":"ai-title","uuid":"x1","aiTitle":"Generated title"}"#,
            r#"{"type":"user","uuid":"u1","parentUuid":null,"customTitle":"stripe-webhooks","message":{"content":"a prompt"}}"#,
        ]);
        assert_eq!(name_last.heading.as_deref(), Some("stripe-webhooks"));
        assert_eq!(name_last.label, "stripe-webhooks");
        assert!(name_last.named);
        assert!(name_last.strong);

        let title_last = scan_lines(&[
            r#"{"type":"user","uuid":"u1","parentUuid":null,"customTitle":"old-name","message":{"content":"a prompt"}}"#,
            r#"{"type":"ai-title","uuid":"x1","aiTitle":"Generated title"}"#,
        ]);
        assert_eq!(title_last.heading.as_deref(), Some("Generated title"));
        // The name is still recorded, it just lost the recency contest.
        assert_eq!(title_last.name.as_deref(), Some("old-name"));
    }

    #[test]
    fn auto_branch_name_falls_back_to_the_first_prompt() {
        let node = scan_lines(&[
            r#"{"type":"user","uuid":"u1","parentUuid":null,"customTitle":"Design payment retry logic (Branch 2)","message":{"content":"handle partial refunds"}}"#,
        ]);
        assert!(node.name.is_none());
        assert!(!node.named);
        assert_eq!(node.label, "handle partial refunds");
    }

    /// A single line carrying an unexpected field type must not drop out of the graph,
    /// or every turn hanging off it would be orphaned too.
    #[test]
    fn a_line_with_an_odd_field_type_still_keeps_its_place() {
        let node = scan_lines(&[
            r#"{"type":"user","uuid":"u1","parentUuid":null,"title":123,"message":{"content":"tolerant parse"}}"#,
        ]);
        assert_eq!(node.prompt_full, "tolerant parse");
    }

    #[test]
    fn tool_result_arrays_are_not_prompts() {
        let node = scan_lines(&[
            r#"{"type":"user","uuid":"u1","parentUuid":null,"message":{"content":[{"type":"tool_result","content":"output"}]}}"#,
            r#"{"type":"user","uuid":"u2","parentUuid":"u1","message":{"content":"the real prompt"}}"#,
        ]);
        assert_eq!(node.prompt_full, "the real prompt");
    }

    #[test]
    fn meta_lines_are_not_prompts() {
        let node = scan_lines(&[
            r#"{"type":"user","uuid":"u1","parentUuid":null,"isMeta":true,"message":{"content":"caveat: the messages below were generated"}}"#,
            r#"{"type":"user","uuid":"u2","parentUuid":"u1","message":{"content":"typed by a human"}}"#,
        ]);
        assert_eq!(node.prompt_full, "typed by a human");
    }

    #[test]
    fn a_root_session_has_no_parent_and_falls_back_to_its_slug() {
        let node = scan_lines(&[r#"{"type":"summary","slug":"payment-retry"}"#]);
        assert!(node.parent.is_none());
        assert!(node.fork_msg.is_none());
        assert_eq!(node.label, "payment-retry");
    }

    #[test]
    fn an_empty_or_unreadable_transcript_degrades_to_its_short_id() {
        let node = scan_lines(&[]);
        assert_eq!(node.label, "cccccccc");
        let missing = scan_session(
            &std::env::temp_dir().join("bg-scan-test-does-not-exist.jsonl"),
            SID,
        );
        assert_eq!(missing.label, "cccccccc");
    }

    #[test]
    fn fork_parent_is_read_from_the_head_of_the_file() {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("bg-probe-{}-{}.jsonl", std::process::id(), n));
        std::fs::write(
            &path,
            "\n{\"uuid\":\"u1\",\"forkedFrom\":{\"sessionId\":\"parent-9\",\"messageUuid\":\"u1\"}}\n",
        )
        .unwrap();
        assert_eq!(first_fork_parent(&path).as_deref(), Some("parent-9"));
        // A root session says nothing in its first lines, which reads as "don't know".
        std::fs::write(&path, "{\"uuid\":\"u1\",\"type\":\"user\"}\n").unwrap();
        assert_eq!(first_fork_parent(&path), None);
        let _ = std::fs::remove_file(&path);
    }
}
