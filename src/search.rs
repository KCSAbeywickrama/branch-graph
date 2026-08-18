//! Local search over the branch list.
//!
//! Matching is entirely offline: no network, no transcript re-read. The haystack is
//! the text `scan_session` already kept per node, so a query costs a string scan.

use std::collections::{HashMap, HashSet};

use crate::model::{Node, Row};
use crate::tree::{build_forest, flatten};

/// fzf-style abbreviation matching, but span-limited: the matched characters must
/// fall inside a window a few times the needle's own length. Unbounded, a 4-char
/// subsequence is findable in almost any sentence ("lcns" hit two thirds of a real
/// branch list), which is the difference between a filter and a shrug. Both operands
/// must already be lowercased.
pub fn subsequence_match(hay: &[char], needle: &[char]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    let max_span = std::cmp::max(needle.len() * 3, needle.len() + 4);
    let first = needle[0];
    // Try every possible start so a tight match later in the string still counts,
    // rather than only the greedy-from-the-left one.
    for start in 0..=(hay.len() - needle.len()) {
        if hay[start] != first {
            continue;
        }
        let mut i = 1usize;
        let mut j = start + 1;
        while j < hay.len() && i < needle.len() && j - start < max_span {
            if hay[j] == needle[i] {
                i += 1;
            }
            j += 1;
        }
        if i == needle.len() {
            return true;
        }
    }
    false
}

/// Two haystacks, because gap-tolerant matching does not survive long text: even
/// span-limited, searching a 4000-char prompt (prompt_full's cap) that way turns up
/// coincidences. Abbreviations are matched only against the SHORT identity fields,
/// while the first prompt is searched by literal substring — precise at any length.
///   short: label/heading/name/title, deduped — where "athflw" -> "auth flow" is
///          the point. Deduping matters: label usually equals heading, and a
///          repeated string doubles the room for a coincidental subsequence.
///   full:  short + first prompt + session id — substring only. Session ids live
///          here alone: a subsequence over hex is noise, and an id is pasted exactly.
pub fn build_haystacks(nodes: &mut [Node]) {
    for n in nodes.iter_mut() {
        let mut parts: Vec<&str> = Vec::with_capacity(4);
        let candidates = [
            Some(n.label.as_str()),
            n.heading.as_deref(),
            n.name.as_deref(),
            n.title.as_deref(),
        ];
        for c in candidates.iter() {
            if let Some(s) = *c {
                if !s.is_empty() && !parts.contains(&s) {
                    parts.push(s);
                }
            }
        }
        let short = parts.join("   ").to_lowercase();
        let full = [
            short.as_str(),
            n.prompt_full.as_str(),
            n.session_id.as_str(),
        ]
        .join("   ")
        .to_lowercase();
        n.hay_short = short.chars().collect();
        n.hay_full = full;
    }
}

/// Every token must hit somewhere, so word order doesn't matter: "auth flow" finds
/// "flow for authentication".
fn match_node(n: &Node, tokens: &[(String, Vec<char>)]) -> bool {
    tokens
        .iter()
        .all(|(t, chars)| n.hay_full.contains(t.as_str()) || subsequence_match(&n.hay_short, chars))
}

/// Rows matching `query`, plus each match's ancestors so the tree still reads as a
/// tree. The pruned set is re-flattened for correct connectors/prefixes, then the
/// original 1-based indices are restored — they line up with the `branch-graph <n>`
/// CLI arg, so rows must keep their real numbers rather than being renumbered.
/// Each returned row carries `matched`: `Some(true)` for a real hit, `Some(false)` for
/// a row kept only as context.
pub fn filter_rows(nodes: &[Node], all_rows: &[Row], query: &str) -> Vec<Row> {
    let lowered = query.to_lowercase();
    let tokens: Vec<(String, Vec<char>)> = lowered
        .split_whitespace()
        .map(|t| (t.to_string(), t.chars().collect()))
        .collect();
    if tokens.is_empty() {
        return all_rows.to_vec();
    }
    let mut idx_by_id: HashMap<&str, usize> = HashMap::with_capacity(all_rows.len());
    for r in all_rows {
        idx_by_id.insert(nodes[r.node].session_id.as_str(), r.node);
    }
    let mut matched: HashSet<usize> = HashSet::new();
    for r in all_rows {
        if match_node(&nodes[r.node], &tokens) {
            matched.insert(r.node);
        }
    }
    if matched.is_empty() {
        return Vec::new();
    }
    let mut keep: HashSet<usize> = HashSet::with_capacity(all_rows.len());
    for &m in matched.iter() {
        // Walk up so a matched leaf keeps the branch it hangs off. Stopping at an
        // already-kept node is safe: we always add a whole chain at once.
        let mut cur = Some(m);
        while let Some(i) = cur {
            if keep.contains(&i) {
                break;
            }
            keep.insert(i);
            cur = nodes[i]
                .effective_parent
                .as_deref()
                .and_then(|p| idx_by_id.get(p).copied());
        }
    }
    // Ancestors of every match are kept too, so build_forest recomputes exactly the
    // same parent links it already had — the subset can never orphan a node.
    // Dropping that invariant would silently break p/← parent navigation.
    let subset: Vec<usize> = all_rows
        .iter()
        .map(|r| r.node)
        .filter(|n| keep.contains(n))
        .collect();
    let mut rows = flatten(&build_forest(nodes, &subset));
    let orig: HashMap<usize, usize> = all_rows.iter().map(|r| (r.node, r.index)).collect();
    for r in rows.iter_mut() {
        if let Some(&i) = orig.get(&r.node) {
            r.index = i;
        }
        r.matched = Some(matched.contains(&r.node));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Row;
    use crate::tree::{build_forest, compute_effective_parents, flatten};

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn subsequence_is_span_limited() {
        // The point of the span limit: "lcns" hits a tight abbreviation...
        assert!(subsequence_match(&chars("choose license"), &chars("lcns")));
        // ...but not letters scattered across a whole sentence.
        assert!(!subsequence_match(
            &chars("look at the connection settings"),
            &chars("lcns")
        ));
    }

    #[test]
    fn subsequence_tries_every_start() {
        // A tight match late in the string counts, not only the greedy-from-left one:
        // the leading "a" of "audit" would strand the greedy walk.
        assert!(subsequence_match(
            &chars("audit auth flow"),
            &chars("athflw")
        ));
    }

    #[test]
    fn subsequence_edges() {
        assert!(subsequence_match(&chars("anything"), &chars("")));
        assert!(!subsequence_match(&chars("ab"), &chars("abc")));
        assert!(!subsequence_match(&chars(""), &chars("a")));
        assert!(subsequence_match(&chars("ab"), &chars("ab")));
    }

    /// label / heading / name / title, deduped, is what abbreviations match against;
    /// the long first prompt is substring-only.
    #[test]
    fn haystacks_dedupe_and_split() {
        let mut nodes = vec![Node::new(
            "11111111-2222-3333-4444-555555555555".to_string(),
        )];
        nodes[0].label = "Stripe webhooks".to_string();
        nodes[0].heading = Some("Stripe webhooks".to_string());
        nodes[0].prompt_full = "handle partial refunds".to_string();
        build_haystacks(&mut nodes);
        let short: String = nodes[0].hay_short.iter().collect();
        assert_eq!(short, "stripe webhooks");
        assert!(nodes[0].hay_full.contains("handle partial refunds"));
        assert!(nodes[0].hay_full.contains("11111111"));
    }

    #[test]
    fn tokens_match_in_any_order() {
        let mut nodes = vec![Node::new(
            "aaaaaaaa-0000-0000-0000-000000000000".to_string(),
        )];
        nodes[0].label = "flow for authentication".to_string();
        build_haystacks(&mut nodes);
        let toks: Vec<(String, Vec<char>)> = "auth flow"
            .split_whitespace()
            .map(|t| (t.to_string(), chars(t)))
            .collect();
        assert!(match_node(&nodes[0], &toks));
    }

    fn node(id: &str, parent: Option<&str>, label: &str, mtime: i64) -> Node {
        let mut n = Node::new(id.to_string());
        n.parent = parent.map(String::from);
        n.label = label.to_string();
        n.mtime = mtime;
        n
    }

    /// The README's example: a match keeps its parent as a greyed-out context row, the
    /// connectors are redrawn for the smaller tree, and rows keep their real numbers so
    /// `branch-graph <n>` still lines up.
    #[test]
    fn filter_keeps_ancestors_as_context() {
        let mut nodes = vec![
            node("a0000000-0-0-0-0", None, "Draft initial README outline", 10),
            node("b0000000-0-0-0-0", None, "Design payment retry logic", 20),
            node(
                "c0000000-0-0-0-0",
                Some("b0000000-0-0-0-0"),
                "stripe-webhooks",
                30,
            ),
            node(
                "d0000000-0-0-0-0",
                Some("b0000000-0-0-0-0"),
                "paypal-webhooks",
                40,
            ),
            node(
                "e0000000-0-0-0-0",
                Some("d0000000-0-0-0-0"),
                "Wire up PayPal IPN handler",
                50,
            ),
        ];
        compute_effective_parents(&mut nodes);
        build_haystacks(&mut nodes);
        let all: Vec<usize> = (0..nodes.len()).collect();
        let all_rows: Vec<Row> = flatten(&build_forest(&nodes, &all));
        let rows = filter_rows(&nodes, &all_rows, "paypal");

        // The parent is carried along, but only the two real hits count as matches.
        let hits = rows.iter().filter(|r| r.matched == Some(true)).count();
        assert_eq!(hits, 2);
        assert_eq!(rows.len(), 3);
        // Original numbering survives filtering.
        let idx: Vec<usize> = rows.iter().map(|r| r.index).collect();
        assert_eq!(idx, vec![2, 4, 5]);
        // Row 2 is context only, and row 4 becomes a `└─` now that its sibling is gone.
        assert_eq!(rows[0].matched, Some(false));
        assert_eq!(rows[1].connector, "└─");
        assert_eq!(rows[2].connector, "└─");
    }

    #[test]
    fn no_matches_yields_no_rows() {
        let mut nodes = vec![node("a0000000-0-0-0-0", None, "only branch", 1)];
        compute_effective_parents(&mut nodes);
        build_haystacks(&mut nodes);
        let all: Vec<usize> = (0..nodes.len()).collect();
        let all_rows = flatten(&build_forest(&nodes, &all));
        assert!(filter_rows(&nodes, &all_rows, "nothingmatchesthis").is_empty());
        // An all-whitespace query is not a filter at all.
        assert_eq!(filter_rows(&nodes, &all_rows, "   ").len(), 1);
    }
}
