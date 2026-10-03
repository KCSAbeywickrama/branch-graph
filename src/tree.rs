//! Building the fork forest, flattening it into drawable rows, and numbering them.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use crate::model::{Lineage, Node, Row};

pub struct Forest {
    /// Node indices with no parent in the set being drawn, oldest first.
    pub roots: Vec<usize>,
    /// parent node index -> child node indices, oldest first.
    pub children: HashMap<usize, Vec<usize>>,
}

/// Place each fork that records no `forkedFrom` — what `claude --resume <id>
/// --fork-session` writes — under the session its copied history came from. Sets
/// `parent` and `fork_msg`; runs before `compute_effective_parents`.
///
/// Such a copy keeps its parent's message uuids, so the leading run of its active path
/// that an OLDER transcript also holds is the copied history, and the run's last message
/// is where it diverged. The parent is the oldest transcript holding that message:
/// siblings forked from the same parent hold it too, but only as copies.
///
/// "Older" means file creation time. Copied lines keep their original timestamps, so
/// nothing inside a transcript tells a copy from its source. Requiring a strictly older
/// parent is also what stops a session's own later forks, which hold its messages too,
/// from passing for its parent, and it rules out cycles. A transcript without a creation
/// time is left as it is.
pub fn infer_copied_parents(nodes: &mut [Node], lineage: &[Lineage]) {
    let is_candidate = |n: &Node| n.parent.is_none() && n.born.is_some();
    // Index only the uuids some candidate's path asks about, not every message in the
    // project.
    let wanted: HashSet<&str> = nodes
        .iter()
        .zip(lineage)
        .filter(|(n, _)| is_candidate(n))
        .flat_map(|(_, l)| l.path.iter().map(String::as_str))
        .collect();
    if wanted.is_empty() {
        return;
    }
    let mut holders: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, l) in lineage.iter().enumerate() {
        if nodes[i].born.is_none() {
            continue;
        }
        for u in &l.uuids {
            if wanted.contains(u.as_str()) {
                holders.entry(u.as_str()).or_default().push(i);
            }
        }
    }

    let mut found: Vec<(usize, String, String)> = Vec::new();
    for (i, l) in lineage.iter().enumerate() {
        let born = match nodes[i].born {
            Some(b) if nodes[i].parent.is_none() => b,
            _ => continue,
        };
        let older = |j: &usize| *j != i && nodes[*j].born.is_some_and(|b| b < born);
        let mut diverged_at: Option<&str> = None;
        for u in &l.path {
            match holders.get(u.as_str()) {
                Some(hs) if hs.iter().any(older) => diverged_at = Some(u),
                _ => break,
            }
        }
        let d = match diverged_at {
            Some(d) => d,
            None => continue,
        };
        let parent = holders[d]
            .iter()
            .filter(|j| older(j))
            .min_by_key(|&&j| nodes[j].born);
        if let Some(&p) = parent {
            found.push((i, nodes[p].session_id.clone(), d.to_string()));
        }
    }
    for (i, parent, d) in found {
        nodes[i].parent = Some(parent);
        nodes[i].fork_msg = Some(d);
    }
}

/// Prefer a fork's own title over a name it merely inherited. A copy can start out with
/// its parent's `custom-title` (`--fork-session` carries it across), and until it is
/// renamed that name only repeats its parent's, while the title Claude Code gives the
/// copy ("<name> ⑂") tells the two apart. The parent may have been renamed since, so any
/// name it ever carried counts. Only the display changes: `name` keeps what the
/// transcript recorded. Runs once parents are settled, recorded or inferred.
pub fn prefer_own_titles(nodes: &mut [Node], lineage: &[Lineage]) {
    let idx_by_id: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.session_id.as_str(), i))
        .collect();
    let inherited: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            let parent_names = n
                .parent
                .as_deref()
                .and_then(|p| idx_by_id.get(p))
                .map(|&p| &lineage[p].names);
            match (n.name.as_ref(), parent_names) {
                (Some(name), Some(names)) => n.title.is_some() && names.contains(name),
                _ => false,
            }
        })
        .map(|(i, _)| i)
        .collect();
    for i in inherited {
        let n = &mut nodes[i];
        n.heading = n.title.clone();
        n.label = n.title.clone().unwrap_or_default();
    }
}

/// Resolve each node's `parent` against the sessions actually present here.
/// A fork whose parent transcript lives in another project becomes a root, which is
/// also what makes `..` able to say so rather than guessing.
pub fn compute_effective_parents(nodes: &mut [Node]) {
    let ids: HashSet<String> = nodes.iter().map(|n| n.session_id.clone()).collect();
    for n in nodes.iter_mut() {
        n.effective_parent = match n.parent.as_ref() {
            Some(p) if ids.contains(p) => Some(p.clone()),
            _ => None,
        };
    }
}

/// Group `subset` (node indices) into a forest by `effective_parent`.
///
/// Siblings and roots are ordered by last-active time, oldest first, so the newest
/// branch is always the last child — which is what `→` (descend) follows. The sort is
/// stable, so equal mtimes keep the order the caller passed them in.
pub fn build_forest(nodes: &[Node], subset: &[usize]) -> Forest {
    let mut idx_by_id: HashMap<&str, usize> = HashMap::with_capacity(subset.len());
    for &i in subset {
        idx_by_id.insert(nodes[i].session_id.as_str(), i);
    }
    let mut roots: Vec<usize> = Vec::new();
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for &i in subset {
        let parent = nodes[i]
            .effective_parent
            .as_deref()
            .and_then(|p| idx_by_id.get(p).copied());
        match parent {
            Some(pi) => children.entry(pi).or_default().push(i),
            None => roots.push(i),
        }
    }
    roots.sort_by_key(|&i| nodes[i].mtime);
    for kids in children.values_mut() {
        kids.sort_by_key(|&i| nodes[i].mtime);
    }
    Forest { roots, children }
}

fn visit(
    forest: &Forest,
    node: usize,
    prefix: &str,
    is_last: bool,
    is_root: bool,
    out: &mut Vec<Row>,
) {
    let connector = if is_root {
        ""
    } else if is_last {
        "└─"
    } else {
        "├─"
    };
    out.push(Row {
        node,
        prefix: prefix.to_string(),
        connector,
        index: 0,
        matched: None,
    });
    let child_prefix = if is_root {
        String::new()
    } else {
        format!("{}{}", prefix, if is_last { "   " } else { "│  " })
    };
    if let Some(kids) = forest.children.get(&node) {
        let last = kids.len() - 1;
        for (i, &k) in kids.iter().enumerate() {
            visit(forest, k, &child_prefix, i == last, false, out);
        }
    }
}

/// DFS over the forest, giving each row its tree art. Rows come back in draw order with
/// `index` still 0: numbering needs the nodes, not just the shape, so `number_by_recency`
/// owns it and is the only thing that ever sets `Row::index`.
pub fn flatten(forest: &Forest) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    let last = forest.roots.len().saturating_sub(1);
    for (i, &r) in forest.roots.iter().enumerate() {
        visit(forest, r, "", i == last, true, &mut out);
    }
    out
}

/// Assign the 1-based number the UI shows and `branch-graph <n>` takes: 1 is the most
/// recently active branch, 2 the one before it, and so on. Numbering by recency instead
/// of by row position makes `1` always mean "the branch I was just in", at the cost of a
/// column that no longer ascends down the tree.
///
/// `rows` arrives in draw order and `sort_by_key` is stable, so branches sharing an mtime
/// are numbered top-to-bottom — the same tie-break `build_forest` uses, and what keeps the
/// numbering deterministic across runs.
pub fn number_by_recency(nodes: &[Node], rows: &mut [Row]) {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| Reverse(nodes[rows[i].node].mtime));
    for (rank, &i) in order.iter().enumerate() {
        rows[i].index = rank + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: Option<&str>, mtime: i64) -> Node {
        let mut n = Node::new(id.to_string());
        n.parent = parent.map(String::from);
        n.mtime = mtime;
        n
    }

    fn build(mut nodes: Vec<Node>) -> (Vec<Node>, Vec<Row>) {
        compute_effective_parents(&mut nodes);
        let all: Vec<usize> = (0..nodes.len()).collect();
        let mut rows = flatten(&build_forest(&nodes, &all));
        number_by_recency(&nodes, &mut rows);
        (nodes, rows)
    }

    /// Oldest first, so the newest branch is always the last child — which is what `→`
    /// (descend to most recent child) relies on.
    #[test]
    fn siblings_and_roots_are_ordered_oldest_first() {
        let (nodes, rows) = build(vec![
            node("root-b", None, 200),
            node("root-a", None, 100),
            node("kid-late", Some("root-a"), 400),
            node("kid-early", Some("root-a"), 300),
        ]);
        let order: Vec<&str> = rows
            .iter()
            .map(|r| nodes[r.node].session_id.as_str())
            .collect();
        assert_eq!(order, vec!["root-a", "kid-early", "kid-late", "root-b"]);
        // Numbers are recency ranks, not row positions: kid-late is the newest, so it is
        // 1 even though it is drawn third, and the column reads 4, 2, 1, 3 down the tree.
        assert_eq!(
            rows.iter().map(|r| r.index).collect::<Vec<_>>(),
            vec![4, 2, 1, 3]
        );
    }

    /// Branches with identical mtimes must not swap numbers between runs, so ties fall
    /// back to draw order.
    #[test]
    fn equal_mtimes_are_numbered_in_draw_order() {
        let (_, rows) = build(vec![
            node("r", None, 10),
            node("a", Some("r"), 10),
            node("b", Some("r"), 10),
        ]);
        assert_eq!(
            rows.iter().map(|r| r.index).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    fn copy(id: &str, born: Option<i64>, path: &[&str], extra: &[&str]) -> (Node, Lineage) {
        let mut n = Node::new(id.to_string());
        n.born = born;
        let lineage = Lineage {
            uuids: path.iter().chain(extra).map(|s| s.to_string()).collect(),
            path: path.iter().map(|s| s.to_string()).collect(),
            names: HashSet::new(),
        };
        (n, lineage)
    }

    /// `--fork-session` copies of P, recognisable only by the message uuids they share.
    #[test]
    fn copies_without_forked_from_are_placed_under_their_source() {
        let (mut nodes, lineage): (Vec<Node>, Vec<Lineage>) = vec![
            // Kept going after both copies were taken, and rewound once: p9 is off-path.
            copy("P", Some(100), &["p1", "p2", "p3", "p4"], &["p9"]),
            // Older sibling, copied at p3 — it holds p2 too, but only as a copy.
            copy("A", Some(150), &["p1", "p2", "p3", "a1"], &[]),
            copy("C", Some(200), &["p1", "p2", "c1"], &[]),
            // A copy of C taken after C's own c1: its parent is C, not P.
            copy("G", Some(300), &["p1", "p2", "c1", "g1"], &[]),
            copy("R", Some(50), &["r1"], &[]),
            // No creation time: nothing to order it by, so it is left alone.
            copy("N", None, &["p1", "p2", "n1"], &[]),
        ]
        .into_iter()
        .unzip();
        infer_copied_parents(&mut nodes, &lineage);
        let got: Vec<(&str, Option<&str>, Option<&str>)> = nodes
            .iter()
            .map(|n| {
                (
                    n.session_id.as_str(),
                    n.parent.as_deref(),
                    n.fork_msg.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                // P's history is held by A, C and G too, but none is older than P.
                ("P", None, None),
                ("A", Some("P"), Some("p3")),
                ("C", Some("P"), Some("p2")),
                ("G", Some("C"), Some("c1")),
                ("R", None, None),
                ("N", None, None),
            ]
        );
    }

    /// A `/branch` fork already names its parent; content overlap must not override it.
    #[test]
    fn a_recorded_forked_from_is_never_overridden() {
        let (mut nodes, lineage): (Vec<Node>, Vec<Lineage>) = vec![
            copy("P", Some(100), &["p1", "p2"], &[]),
            copy("B", Some(200), &["p1", "p2", "b1"], &[]),
        ]
        .into_iter()
        .unzip();
        nodes[1].parent = Some("elsewhere".to_string());
        infer_copied_parents(&mut nodes, &lineage);
        assert_eq!(nodes[1].parent.as_deref(), Some("elsewhere"));
        assert!(nodes[1].fork_msg.is_none());
    }

    fn named(id: &str, parent: Option<&str>, name: &str, title: Option<&str>) -> Node {
        let mut n = node(id, parent, 0);
        n.name = Some(name.to_string());
        n.title = title.map(String::from);
        n.heading = n.name.clone();
        n.label = name.to_string();
        n
    }

    #[test]
    fn an_inherited_name_gives_way_to_the_forks_own_title() {
        let mut nodes = vec![
            // Renamed since the copies were taken.
            named("P", None, "smee diagnose", Some("Generated")),
            // Still carrying the name P had when it was copied: show its own title.
            named("C", Some("P"), "live progress", Some("live progress ⑂")),
            // Renamed after forking: the name is its own and wins.
            named("R", Some("P"), "live progress v2", Some("live progress ⑂")),
            // Inherited, but no title to show instead.
            named("U", Some("P"), "live progress", None),
        ];
        let mut lineage: Vec<Lineage> = (0..nodes.len()).map(|_| Lineage::default()).collect();
        lineage[0].names = ["live progress", "smee diagnose"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        prefer_own_titles(&mut nodes, &lineage);
        let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "smee diagnose",
                "live progress ⑂",
                "live progress v2",
                "live progress"
            ]
        );
        assert_eq!(nodes[1].heading.as_deref(), Some("live progress ⑂"));
        assert_eq!(nodes[1].name.as_deref(), Some("live progress"));
    }

    /// A fork whose parent transcript lives in another project cannot be drawn under it,
    /// so it becomes a root — and `parent` is what later tells that apart from a true root.
    #[test]
    fn a_fork_with_an_absent_parent_becomes_a_root() {
        let (nodes, rows) = build(vec![node("orphan", Some("elsewhere"), 10)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].connector, "");
        assert!(nodes[0].effective_parent.is_none());
        assert_eq!(nodes[0].parent.as_deref(), Some("elsewhere"));
    }

    #[test]
    fn tree_art_matches_nesting() {
        let (nodes, rows) = build(vec![
            node("r", None, 10),
            node("a", Some("r"), 20),
            node("b", Some("r"), 30),
            node("a1", Some("a"), 40),
            node("b1", Some("b"), 50),
        ]);
        let art: Vec<String> = rows
            .iter()
            .map(|r| format!("{}{}{}", r.prefix, r.connector, nodes[r.node].session_id))
            .collect();
        assert_eq!(
            art,
            vec![
                "r".to_string(),
                "├─a".to_string(),
                "│  └─a1".to_string(),
                "└─b".to_string(),
                "   └─b1".to_string(),
            ]
        );
    }
}
