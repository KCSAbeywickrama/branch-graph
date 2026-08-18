//! Shared data model: one `Node` per session transcript, one `Row` per drawn line.

/// A session transcript, after scanning.
///
/// Field names mirror the JS original so the two implementations can be diffed
/// against each other field by field.
pub struct Node {
    pub session_id: String,
    /// `forkedFrom.sessionId` as recorded in the transcript, whether or not that
    /// session has a transcript in this project.
    pub parent: Option<String>,
    /// Divergence message: the message in the parent this branch forked at.
    pub fork_msg: Option<String>,
    /// What the row shows: heading, else first prompt, else slug, else short id.
    pub label: String,
    /// Explicit user-set name (`/branch <name>`, `/rename`); auto "(Branch N)"
    /// names are not counted.
    pub name: Option<String>,
    pub named: bool,
    /// Claude's own summary (`aiTitle`), which `/rename` on a live session also writes.
    pub title: Option<String>,
    /// The winning name/title: whichever of the two landed last in the transcript.
    pub heading: Option<String>,
    /// First prompt typed on this branch (capped), for the detail panel and search.
    pub prompt_full: String,
    /// True when `label` came from a name/title (drawn bold) rather than a prompt.
    pub strong: bool,
    /// Transcript mtime in epoch milliseconds: this branch's last-active time.
    pub mtime: i64,
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
            current: false,
            latest: false,
            effective_parent: None,
            hay_short: Vec::new(),
            hay_full: String::new(),
        }
    }
}

/// One line of the drawn tree: which node, and the tree art leading to it.
#[derive(Clone)]
pub struct Row {
    /// Index into the `nodes` slice.
    pub node: usize,
    pub prefix: String,
    pub connector: &'static str,
    /// Stable 1-based index over the FULL tree. Filtered views keep the original
    /// number, because it is what `branch-graph <n>` takes.
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
