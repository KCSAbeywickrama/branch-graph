//! Shared data model: one `Node` per session transcript, one `Row` per drawn line.

use std::collections::HashSet;

/// A session transcript, after scanning.
///
/// Field names mirror the JS original so the two implementations can be diffed
/// against each other field by field.
pub struct Node {
    pub session_id: String,
    /// The session this one forked from, whether or not that session has a transcript
    /// in this project: `forkedFrom.sessionId` as recorded in the transcript, or, for a
    /// `--fork-session` copy that records none, the one `tree::infer_copied_parents`
    /// traces its copied history back to.
    pub parent: Option<String>,
    /// Divergence message: the message in the parent this branch forked at.
    pub fork_msg: Option<String>,
    /// What the row shows: heading, else first prompt, else slug, else short id.
    pub label: String,
    /// Explicit user-set name (`/branch <name>`, `/rename`); auto "(Branch N)"
    /// names are not counted.
    pub name: Option<String>,
    pub named: bool,
    /// Claude's own summary (`aiTitle`).
    pub title: Option<String>,
    /// The winning name/title: the explicit name when there is one, else the title.
    pub heading: Option<String>,
    /// First prompt typed on this branch (capped), for the detail panel and search.
    pub prompt_full: String,
    /// True when `label` came from a name/title (drawn bold) rather than a prompt.
    pub strong: bool,
    /// Transcript mtime in epoch milliseconds: this branch's last-active time.
    pub mtime: i64,
    /// Transcript creation time in epoch milliseconds, which for a fork is when it was
    /// made. None where the filesystem keeps no birth time.
    pub born: Option<i64>,
    /// The session we are running inside, when it belongs to this project.
    pub current: bool,
    /// Most recently written session, used as the anchor when `current` is unknown.
    pub latest: bool,
    /// `parent`, but only when that session has a transcript here; otherwise None,
    /// which makes the node a root of the drawn forest.
    pub effective_parent: Option<String>,
    /// Search haystacks, precomputed once. `short` is char-indexed for
    /// abbreviation matching; `full` is scanned as a substring. See `search`.
    pub hay_short: Vec<char>,
    pub hay_full: String,
}

impl Node {
    pub fn new(session_id: String) -> Node {
        Node {
            session_id,
            parent: None,
            fork_msg: None,
            label: String::new(),
            name: None,
            named: false,
            title: None,
            heading: None,
            prompt_full: String::new(),
            strong: false,
            mtime: 0,
            born: None,
            current: false,
            latest: false,
            effective_parent: None,
            hay_short: Vec::new(),
            hay_full: String::new(),
        }
    }
}

/// What relates a transcript to its parent: the message ids it holds, which place a fork
/// that carries no `forkedFrom` (`tree::infer_copied_parents`), and the names it has
/// carried, which tell a fork's inherited name from its own (`tree::prefer_own_titles`).
/// Kept beside the `Node` rather than in it, so it can be dropped once both are settled.
#[derive(Default)]
pub struct Lineage {
    /// Every message uuid in the transcript, on the active path or not.
    pub uuids: HashSet<String>,
    /// The active path, root first, followed across compaction boundaries. Left empty
    /// when the transcript names its parent in `forkedFrom`, since nothing is inferred.
    pub path: Vec<String>,
    /// Every explicit name the transcript recorded, cleaned like `Node::name` — not just
    /// the latest, since a parent renamed after the fork no longer matches its copy.
    pub names: HashSet<String>,
}

/// One line of the drawn tree: which node, and the tree art leading to it.
#[derive(Clone)]
pub struct Row {
    /// Index into the `nodes` slice.
    pub node: usize,
    pub prefix: String,
    pub connector: &'static str,
    /// 1-based recency rank over the FULL tree: 1 is the most recently active branch,
    /// 2 the one before it. Not a row position, so the column does not ascend down the
    /// tree. Filtered views keep the original number, because it is what
    /// `branch-graph <n>` takes. Assigned only by `tree::number_by_recency`.
    pub index: usize,
    /// `None` in an unfiltered list; `Some(true)` for a search hit, `Some(false)`
    /// for a row kept only as context. So `!= Some(false)` reads as "not a context
    /// row" in both cases.
    pub matched: Option<bool>,
}

/// First 8 chars of a session id / uuid, the form shown everywhere in the UI.
pub fn short_id(s: &str) -> &str {
    match s.char_indices().nth(8) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}
