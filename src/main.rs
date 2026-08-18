//! branch-graph — visualize the Claude Code session fork tree (zero LLM tokens).
//!
//! Claude Code's `/branch` (and `--fork-session`) creates a NEW session whose JSONL
//! lines carry `forkedFrom: { sessionId, messageUuid }`. This tool reads every session
//! transcript for the current project, reconstructs the fork tree, highlights the
//! current session, and surfaces a ready `/resume <id>` for each branch so you can
//! switch in place inside the running Claude Code instance.
//!
//! Usage (inside Claude Code, zero tokens):
//!   !branch-graph            list the fork tree with a /resume line per branch
//!   !branch-graph <n>        print only the /resume line for branch number <n>
//!   !branch-graph ..         jump to the parent of the most recent branch

mod model;
mod render;
mod scan;
mod search;
mod term;
mod tree;
mod tui;

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use model::{short_id, Node};
use render::{dim, resume_line, truncate};

// ---------- args ----------

struct Opts {
    project: Option<String>,
    json: bool,
    list: bool,
    help: bool,
    index: Option<usize>,
    /// The raw `<n>` token when it does not fit in a usize: no such branch can exist,
    /// but the diagnostic still wants the digits the user typed.
    index_overflow: Option<String>,
    color: Option<bool>,
    interactive: Option<bool>,
    debug_mouse: bool,
    parent: bool,
}

fn parse_args(argv: &[String]) -> Opts {
    let mut opts = Opts {
        project: None,
        json: false,
        list: false,
        help: false,
        index: None,
        index_overflow: None,
        color: None,
        interactive: None,
        debug_mouse: false,
        parent: false,
    };
    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].as_str();
        match a {
            "-h" | "--help" => opts.help = true,
            // `..` reads as "up one level" like `cd ..`, and unlike `^` no shell mangles it.
            ".." | "-p" | "--parent" | "--up" => opts.parent = true,
            "--debug-mouse" => opts.debug_mouse = true,
            "--json" => opts.json = true,
            "--list" => opts.list = true,
            "-i" | "--interactive" => opts.interactive = Some(true),
            "--no-interactive" => opts.interactive = Some(false),
            "--color" => opts.color = Some(true),
            "--no-color" => opts.color = Some(false),
            "--project" => {
                opts.project = argv.get(i + 1).cloned();
                i += 1;
            }
            _ => {
                if !a.is_empty() && a.bytes().all(|b| b.is_ascii_digit()) {
                    match a.parse::<usize>() {
                        Ok(v) => opts.index = Some(v),
                        // All-digits, so the only way to fail is overflow. Remember it
                        // as a branch request that cannot match, and let the usual
                        // "no branch #n (valid: 1-N)" path report it.
                        Err(_) => opts.index_overflow = Some(a.to_string()),
                    }
                } else {
                    eprintln!("branch-graph: unknown argument \"{}\"", a);
                    std::process::exit(2);
                }
            }
        }
        i += 1;
    }
    opts
}

/// An env var that is set and non-empty, matching JS truthiness on `process.env.X`.
fn env_truthy(k: &str) -> bool {
    std::env::var(k).map(|v| !v.is_empty()).unwrap_or(false)
}

/// Color is on when: --color or FORCE_COLOR; off when --no-color or NO_COLOR;
/// otherwise auto. Auto = on in a real terminal AND inside Claude Code (which sets
/// CLAUDECODE=1 even though the `!` prefix pipes stdout). Plain pipes/redirects to
/// files stay color-free.
fn decide_color(opts: &Opts) -> bool {
    if opts.color == Some(false) || env_truthy("NO_COLOR") {
        return false;
    }
    if opts.color == Some(true) || env_truthy("FORCE_COLOR") {
        return true;
    }
    term::isatty(libc::STDOUT_FILENO) || env_truthy("CLAUDECODE")
}

// ---------- project dir resolution ----------

/// Claude Code maps a cwd to a project dir name by replacing `/` and `.` with `-`.
fn munge_cwd(p: &str) -> String {
    p.chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Absolute, lexically normalized path — the same shape Node's `path.resolve` returns
/// (symlinks are not followed, so the name Claude Code munged stays the name we munge).
fn resolve_path(p: &str) -> PathBuf {
    let path = Path::new(p);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd().join(path)
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

fn resolve_project_dir(project: Option<&str>) -> PathBuf {
    let projects = home_dir().join(".claude").join("projects");
    let sep = std::path::MAIN_SEPARATOR;
    match project {
        // Either a direct projects/<dir> path, or a working dir to munge.
        Some(p) => {
            if p.contains(&format!(".claude{}projects", sep)) {
                return PathBuf::from(p);
            }
            let pb = PathBuf::from(p);
            if pb.is_dir() && p.contains(&format!("{}.claude{}", sep, sep)) {
                return pb;
            }
            projects.join(munge_cwd(&resolve_path(p).to_string_lossy()))
        }
        None => projects.join(munge_cwd(&cwd().to_string_lossy())),
    }
}

// ---------- resuming ----------

/// Hand the terminal over to a Claude session on `session_id`. Never returns. Callers
/// holding terminal state (the picker) must restore it first.
pub fn launch_resume(session_id: &str) -> ! {
    match std::process::Command::new("claude")
        .arg("-r")
        .arg(session_id)
        .status()
    {
        Ok(status) => std::process::exit(status.code().unwrap_or(0)),
        Err(e) => {
            // `claude` missing or not executable — fall back to the paste-able line
            // rather than exiting silently as if the switch had happened.
            eprintln!("branch-graph: could not run `claude` ({})", e);
            println!("{}", resume_line(session_id));
            std::process::exit(1);
        }
    }
}

/// Go up one fork level: hand the terminal to the parent branch, or print the paste-able
/// line when we can't own the terminal. Inside Claude Code's piped `!` a nested `claude`
/// would be wrong, so that case prints too. Shared by both `..` paths so the two can't
/// drift apart.
fn go_to_parent(parent_id: &str, opts: &Opts, can_interact: bool) {
    let can_launch =
        can_interact && !env_truthy("CLAUDECODE") && opts.interactive != Some(false) && !opts.list;
    if can_launch {
        launch_resume(parent_id); // never returns
    }
    println!("{}", resume_line(parent_id));
    println!(
        "{}",
        dim(&format!("(other terminal: claude -r {})", parent_id))
    );
}

// ---------- json ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonRow<'a> {
    index: usize,
    session_id: &'a str,
    parent: Option<&'a str>,
    fork_message_uuid: Option<&'a str>,
    label: &'a str,
    name: Option<&'a str>,
    named: bool,
    title: Option<&'a str>,
    first_prompt: Option<&'a str>,
    current: bool,
    latest: bool,
    resume: Option<String>,
}

fn help() -> String {
    [
        "branch-graph — visualize Claude Code session fork tree (zero tokens)",
        "",
        "Usage:",
        "  !branch-graph            list the fork tree with a /resume line per branch",
        "  !branch-graph <n>        print only the /resume line for branch number <n>",
        "  !branch-graph ..         go up one fork level from the most recent branch",
        "",
        "In a real terminal it opens an interactive picker (↑/↓ or mouse to navigate, p/← to",
        "jump to the parent branch and → to its most recent child, Enter/click to resume that",
        "branch). Piped or inside Claude Code it prints a list.",
        "",
        "Search (picker only):",
        "  s or /             start typing a query; the tree filters as you type, keeping",
        "                     each match's parent branches greyed out for context. The",
        "                     selection lands on the first real match.",
        "  Enter              accept the filter and go back to navigating it",
        "  Esc                cancel typing / clear the filter / quit (one layer per press)",
        "",
        "  Matching runs locally over branch names, titles, first prompts and session ids",
        "  (not full transcript bodies). It is case-insensitive and word order does not",
        "  matter, so \"auth flow\" finds \"flow for authentication\". Against names and titles",
        "  you can also skip letters — \"lcns\" finds \"Choose license\" — while first prompts",
        "  match on text you actually typed.",
        "",
        "Flags:",
        "  .., -p, --parent   jump to the parent of the most recent branch: launches",
        "                     `claude -r` in a terminal, prints /resume when piped or",
        "                     inside Claude Code. Takes precedence over the picker.",
        "  -i, --interactive  force the interactive picker (requires a terminal)",
        "  --no-interactive   force plain list output",
        "  --project <path>   a working dir or ~/.claude/projects/<dir> to inspect",
        "  --json             machine-readable output",
        "  --list             force list output",
        "  --color/--no-color force or disable ANSI color (auto-on in a terminal & Claude Code)",
        "  -h, --help         show this help",
    ]
    .join("\n")
}

// ---------- main ----------

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let opts = parse_args(&argv);
    render::set_color(decide_color(&opts));
    if opts.help {
        println!("{}", help());
        return;
    }
    // A panic while the picker owns the terminal must still hand it back, or the shell
    // is left in raw mode with mouse reporting on.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        term::restore();
        prev_hook(info);
    }));
    if opts.debug_mouse {
        tui::debug_mouse();
    }

    let dir = resolve_project_dir(opts.project.as_deref());
    if !dir.is_dir() {
        eprintln!(
            "branch-graph: no sessions found for this project\n  (looked in: {})",
            dir.display()
        );
        std::process::exit(1);
    }

    let read = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!("branch-graph: cannot read {} ({})", dir.display(), e);
            std::process::exit(1);
        }
    };
    // Everything derivable from the directory listing alone, before a byte of any
    // transcript is parsed: which sessions live here, and which was written last. The
    // `..` fast path below runs on these; the scan loop reuses the same mtimes so both
    // paths agree on which session is newest.
    let mut entries: Vec<(PathBuf, String, i64)> = Vec::new();
    let mut newest_file: Option<String> = None;
    let mut newest_mtime: i64 = -1;
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".jsonl") {
            continue;
        }
        let path = entry.path();
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let session_id = name[..name.len() - ".jsonl".len()].to_string();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if mtime > newest_mtime {
            newest_mtime = mtime;
            newest_file = Some(session_id.clone());
        }
        entries.push((path, session_id, mtime));
    }
    if entries.is_empty() {
        eprintln!("branch-graph: no session transcripts in {}", dir.display());
        std::process::exit(1);
    }
    let ids_here: HashSet<String> = entries.iter().map(|e| e.1.clone()).collect();

    let current_id: Option<String> = std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty());
    // "current" = the session we're running inside, but only if it belongs to the
    // project being shown. Otherwise we can't truthfully claim one is "this session" —
    // fall back to flagging the most recently written session as "most recent" so there
    // is always a useful anchor.
    let exact_match = current_id
        .as_ref()
        .map(|id| ids_here.contains(id))
        .unwrap_or(false);
    // The current session is in another project.
    let elsewhere = current_id.is_some() && !exact_match;
    // A real terminal on both ends is what lets us hand the terminal over — to
    // `claude -r` for `..`, or to the picker further down.
    let can_interact = term::isatty(libc::STDOUT_FILENO) && term::isatty(libc::STDIN_FILENO);
    // The anchor for `..`: the session we're running inside when it belongs to this
    // project, else the most recently written one — the same precedence as the
    // current/latest markers, so `..` means "up from where I just was".
    let anchor_id: Option<String> = if exact_match {
        current_id.clone()
    } else {
        newest_file.clone()
    };

    // `..` fast path. Going up needs exactly two facts — the anchor and what it forked
    // from — and both are cheap, so answer without parsing the project. --json and a
    // branch number describe the whole tree, so those keep the full scan. Anything
    // `first_fork_parent` can't settle (root, or a parent whose transcript lives in
    // another project) falls through to the scan below, which owns the diagnostics.
    if opts.parent && !opts.json && opts.index.is_none() {
        if let Some(anchor) = anchor_id.as_deref() {
            let file = dir.join(format!("{}.jsonl", anchor));
            if let Some(parent_id) = scan::first_fork_parent(&file) {
                if ids_here.contains(&parent_id) {
                    go_to_parent(&parent_id, &opts, can_interact);
                    return;
                }
            }
        }
    }

    // Transcripts are independent, so the scan fans out across cores and each thread
    // keeps its chunk in the caller's order — the tree's tie-breaking stays stable.
    let n_threads = std::thread::available_parallelism()
        .map(|v| v.get())
        .unwrap_or(4)
        .min(entries.len())
        .max(1);
    let chunk_size = (entries.len() + n_threads - 1) / n_threads;
    let mut nodes: Vec<Node> = Vec::with_capacity(entries.len());
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for chunk in entries.chunks(chunk_size) {
            handles.push(s.spawn(move || {
                chunk
                    .iter()
                    .map(|(path, sid, mtime)| {
                        let mut n = scan::scan_session(path, sid);
                        n.mtime = *mtime;
                        n
                    })
                    .collect::<Vec<Node>>()
            }));
        }
        for h in handles {
            if let Ok(part) = h.join() {
                nodes.extend(part);
            }
        }
    });

    for n in nodes.iter_mut() {
        n.current = exact_match && current_id.as_deref() == Some(n.session_id.as_str());
        n.latest = !exact_match && newest_file.as_deref() == Some(n.session_id.as_str());
    }
    tree::compute_effective_parents(&mut nodes);
    search::build_haystacks(&mut nodes);
    let all: Vec<usize> = (0..nodes.len()).collect();
    let rows = tree::flatten(&tree::build_forest(&nodes, &all));

    let project_name = match opts.project.as_deref() {
        Some(p) => resolve_path(p)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        None => cwd()
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
    };

    if opts.json {
        let out: Vec<JsonRow> = rows
            .iter()
            .map(|r| {
                let n = &nodes[r.node];
                JsonRow {
                    index: r.index,
                    session_id: &n.session_id,
                    parent: n.effective_parent.as_deref(),
                    fork_message_uuid: n.fork_msg.as_deref(),
                    label: &n.label,
                    name: n.name.as_deref(),
                    named: n.named,
                    title: n.title.as_deref(),
                    first_prompt: if n.prompt_full.is_empty() {
                        None
                    } else {
                        Some(n.prompt_full.as_str())
                    },
                    current: n.current,
                    latest: n.latest,
                    resume: if n.current {
                        None
                    } else {
                        Some(resume_line(&n.session_id))
                    },
                }
            })
            .collect();
        match serde_json::to_string_pretty(&out) {
            Ok(s) => println!("{}", s),
            Err(e) => {
                eprintln!("branch-graph: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    if let Some(raw) = opts.index_overflow.as_deref() {
        eprintln!("branch-graph: no branch #{} (valid: 1-{})", raw, rows.len());
        std::process::exit(1);
    }

    if let Some(idx) = opts.index {
        let row = match rows.iter().find(|r| r.index == idx) {
            Some(r) => r,
            None => {
                eprintln!("branch-graph: no branch #{} (valid: 1-{})", idx, rows.len());
                std::process::exit(1);
            }
        };
        let n = &nodes[row.node];
        if n.current {
            println!("Branch #{} is the current session — already here.", idx);
            return;
        }
        println!("{}", resume_line(&n.session_id));
        println!(
            "{}",
            dim(&format!("(other terminal: claude -r {})", n.session_id))
        );
        return;
    }

    // `..` / -p: go up one fork level. Only the cases the fast path above declined reach
    // here — a root, or a parent transcript that lives in another project — plus --json
    // and <n>, which returned already. So this is now the diagnostic path.
    if opts.parent {
        let anchor_id = match anchor_id.as_deref() {
            Some(a) => a,
            None => {
                eprintln!("branch-graph: could not determine the most recent branch");
                std::process::exit(1);
            }
        };
        let anchor = match nodes.iter().find(|n| n.session_id == anchor_id) {
            Some(a) => a,
            None => {
                eprintln!("branch-graph: could not determine the most recent branch");
                std::process::exit(1);
            }
        };
        // `compute_effective_parents` nulls the link both for a true root and for a fork
        // whose parent transcript lives elsewhere; `parent` is what tells them apart.
        let effective = match anchor.effective_parent.clone() {
            Some(p) => p,
            None => {
                match anchor.parent.as_deref() {
                    Some(p) => eprintln!(
                        "branch-graph: parent {} of {} has no transcript in this project",
                        short_id(p),
                        short_id(anchor_id)
                    ),
                    None => eprintln!(
                        "branch-graph: most recent branch {} ({}) is a root — no parent branch",
                        short_id(anchor_id),
                        truncate(&anchor.label, 40)
                    ),
                }
                std::process::exit(1);
            }
        };
        go_to_parent(&effective, &opts, can_interact);
        return;
    }

    // Interactive TUI: only in a real terminal, never inside Claude Code's piped `!`.
    if opts.interactive == Some(true) && !can_interact {
        eprintln!("branch-graph: --interactive requires a terminal (TTY)");
        std::process::exit(2);
    }
    let want_interactive = can_interact
        && (opts.interactive == Some(true)
            || (opts.interactive != Some(false) && !env_truthy("CLAUDECODE") && !opts.list));
    if want_interactive {
        // Hands off the terminal (resume) or exits on quit; never returns.
        tui::run(
            &nodes,
            rows,
            tui::Ctx {
                project_name: project_name.clone(),
                elsewhere,
                current_id: current_id.clone(),
            },
        );
    }

    println!(
        "{}",
        render::render_list(
            &nodes,
            &rows,
            &project_name,
            &render::Meta {
                elsewhere,
                current_id,
            }
        )
    );
}
