//! Building the fork forest and flattening it into drawable rows.

use std::collections::{HashMap, HashSet};

use crate::model::{Node, Row};

pub struct Forest {
    /// Node indices with no parent in the set being drawn, oldest first.
    pub roots: Vec<usize>,
    /// parent node index -> child node indices, oldest first.
    pub children: HashMap<usize, Vec<usize>>,
}

/// Resolve each node's `forkedFrom` against the sessions actually present here.
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

/// DFS over the forest, assigning stable 1-based indices and the tree art per row.
pub fn flatten(forest: &Forest) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    let last = forest.roots.len().saturating_sub(1);
    for (i, &r) in forest.roots.iter().enumerate() {
        visit(forest, r, "", i == last, true, &mut out);
    }
    for (i, row) in out.iter_mut().enumerate() {
        row.index = i + 1;
    }
    out
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
        let rows = flatten(&build_forest(&nodes, &all));
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
        // Indices are assigned in draw order, 1-based.
        assert_eq!(
            rows.iter().map(|r| r.index).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
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
