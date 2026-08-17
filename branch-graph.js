#!/usr/bin/env node
'use strict';

// branch-graph — visualize Claude Code session fork tree (zero LLM tokens).
//
// Claude Code's `/branch` (and `--fork-session`) creates a NEW session whose
// JSONL lines carry `forkedFrom: { sessionId, messageUuid }`. This tool reads
// every session transcript for the current project, reconstructs the fork tree,
// highlights the current session, and surfaces a ready `/resume <id>` for each
// branch so you can switch in place inside the running Claude Code instance.
//
// Usage (inside Claude Code, zero tokens):
//   !branch-graph            list the fork tree with a /resume line per branch
//   !branch-graph <n>        print only the /resume line for branch number <n>
//   !branch-graph ..         jump to the parent of the most recent branch
// Flags:
//   .., -p, --parent         go up one fork level from the most recent branch
//   --project <path>         use a specific ~/.claude/projects/<dir> (or a cwd)
//   --json                   machine-readable output
//   --list                   force list output (default when no <n>)
//   -h, --help               this help

const fs = require('fs');
const os = require('os');
const path = require('path');
const readline = require('readline');

// ---------- args ----------
function parseArgs(argv) {
  const opts = { project: null, json: false, list: false, help: false, index: null,
    color: null, interactive: null, debugMouse: false, parent: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '-h' || a === '--help') opts.help = true;
    // `..` reads as "up one level" like `cd ..`, and unlike `^` no shell mangles it.
    else if (a === '..' || a === '-p' || a === '--parent' || a === '--up') opts.parent = true;
    else if (a === '--debug-mouse') opts.debugMouse = true;
    else if (a === '--json') opts.json = true;
    else if (a === '--list') opts.list = true;
    else if (a === '-i' || a === '--interactive') opts.interactive = true;
    else if (a === '--no-interactive') opts.interactive = false;
    else if (a === '--color') opts.color = true;
    else if (a === '--no-color') opts.color = false;
    else if (a === '--project') opts.project = argv[++i];
    else if (/^\d+$/.test(a)) opts.index = parseInt(a, 10);
    else { process.stderr.write(`branch-graph: unknown argument "${a}"\n`); process.exit(2); }
  }
  return opts;
}

// Color is on when: --color or FORCE_COLOR; off when --no-color or NO_COLOR;
// otherwise auto. Auto = on in a real terminal AND inside Claude Code (which sets
// CLAUDECODE=1 even though the `!` prefix pipes stdout). Plain pipes/redirects to
// files stay color-free.
function decideColor(opts) {
  if (opts.color === false || process.env.NO_COLOR) return false;
  if (opts.color === true || process.env.FORCE_COLOR) return true;
  return Boolean(process.stdout.isTTY) || Boolean(process.env.CLAUDECODE);
}

// ---------- colors ----------
// Set in main() from decideColor(); the helpers below read it at call time.
let useColor = false;
const c = (code, s) => (useColor ? `\x1b[${code}m${s}\x1b[0m` : s);
const dim = (s) => c('2', s);
const bold = (s) => c('1', s);
const cyan = (s) => c('36', s);
const green = (s) => c('32', s);
const yellow = (s) => c('33', s);

// ---------- project dir resolution ----------
function mungeCwd(p) {
  // Claude Code maps a cwd to a project dir name by replacing `/` and `.` with `-`.
  return p.replace(/[/.]/g, '-');
}
function resolveProjectDir(project) {
  if (project) {
    // Either a direct projects/<dir> path, or a working dir to munge.
    if (project.includes(path.join('.claude', 'projects'))) return project;
    if (fs.existsSync(project) && fs.statSync(project).isDirectory() &&
        project.includes(path.sep + '.claude' + path.sep)) return project;
    return path.join(os.homedir(), '.claude', 'projects', mungeCwd(path.resolve(project)));
  }
  return path.join(os.homedir(), '.claude', 'projects', mungeCwd(process.cwd()));
}

// ---------- label cleanup ----------
function cleanPrompt(str) {
  return str
    // eslint-disable-next-line no-control-regex
    .replace(/\x1b\[[0-9;]*m/g, '') // strip ANSI color (e.g. pasted tool output)
    .replace(/<command-[^>]*>[\s\S]*?<\/command-[^>]*>/g, '')
    .replace(/<local-command-[^>]*>[\s\S]*?<\/local-command-[^>]*>/g, '')
    .replace(/<[^>]+>/g, '')
    .replace(/\s+/g, ' ')
    .trim();
}
function truncate(s, n) {
  return s.length > n ? s.slice(0, n - 1) + '…' : s;
}
// Claude Code auto-names a `/branch` (created without a name) as "<…> (Branch N)".
// We only treat an EXPLICIT `/branch myname` name as a real name, so auto names are
// ignored and those branches fall back to their first prompt.
const AUTO_NAME_RE = /\(Branch(\s+\d+)?\)\s*$/;

// ---------- scan one session file ----------
//
// A `/branch` fork replays the parent's history into the new file, tagging every
// copied line with `forkedFrom` (whose `messageUuid` equals that line's own uuid).
// The branch's OWN messages have no `forkedFrom`.
//
// Within a single session, rewinding to an earlier prompt and retyping appends a
// NEW sibling: the old turns stay earlier in the file but become orphaned, and the
// new turns hang off the same parent. So we must not pick prompts by file order —
// we follow the ACTIVE path (current leaf back to root) and read the first own
// prompt along it. Claude Code records the live head as the latest `last-prompt`
// line's `leafUuid`; we fall back to the last message in the file.
async function scanSession(file, sessionId) {
  const info = { sessionId, parent: null, forkMsg: null, label: null,
    name: null, named: false, title: null, heading: null, promptFull: '' };
  let title = null, slug = null, name = null;
  // Line numbers of the last title/name write, so we can tell which one is newer.
  let titleAt = -1, nameAt = -1, lineNo = 0;
  let lastForkMsg = null;        // messageUuid of the last replayed line = divergence leaf
  let leafUuid = null, lastUuid = null;
  const nodes = new Map();       // uuid -> { parent, type, isMeta, fork, content }
  const rl = readline.createInterface({
    input: fs.createReadStream(file, { encoding: 'utf8' }),
    crlfDelay: Infinity,
  });
  for await (const line of rl) {
    lineNo++;
    if (!line) continue;
    let o;
    try { o = JSON.parse(line); } catch { continue; }
    if (o.type === 'last-prompt' && o.leafUuid) leafUuid = o.leafUuid; // live head pointer
    if (o.forkedFrom && o.forkedFrom.sessionId) {
      if (!info.parent) info.parent = o.forkedFrom.sessionId;
      if (o.forkedFrom.messageUuid) lastForkMsg = o.forkedFrom.messageUuid;
    }
    // Claude's own summary. NOTE: `/rename` on the LIVE session is also recorded here
    // (it overwrites aiTitle), so this field is not purely machine-generated.
    if (typeof o.aiTitle === 'string' && o.aiTitle.trim()) { title = o.aiTitle.trim(); titleAt = lineNo; }
    else if (o.type === 'ai-title') {
      const t = o.title || o.aiTitle || o.content;
      if (typeof t === 'string' && t.trim()) { title = t.trim(); titleAt = lineNo; }
    }
    // User-set display name (the one shown in /resume), from `/rename` or `/branch
    // <name>`. Last EXPLICIT write wins: auto "<parent> (Branch N)" names are skipped
    // here so a later auto write can't clobber a name the user actually typed.
    if (typeof o.customTitle === 'string' && o.customTitle.trim()) {
      const t = o.customTitle.trim();
      if (!AUTO_NAME_RE.test(t)) { name = t; nameAt = lineNo; }
    }
    if (!slug && typeof o.slug === 'string') slug = o.slug;
    if (o.uuid) {
      lastUuid = o.uuid;
      const content = (o.type === 'user' && !o.isMeta && o.message &&
        typeof o.message.content === 'string') ? o.message.content.slice(0, 4096) : null;
      nodes.set(o.uuid, { parent: o.parentUuid || null, type: o.type,
        isMeta: !!o.isMeta, fork: !!o.forkedFrom, content });
    }
  }

  // Walk the active path: current leaf -> root, then read it root-first.
  const head = (leafUuid && nodes.has(leafUuid)) ? leafUuid : lastUuid;
  const path = [];
  const seen = new Set();
  for (let cur = head; cur && nodes.has(cur) && !seen.has(cur); cur = nodes.get(cur).parent) {
    seen.add(cur);
    path.push(cur);
  }
  path.reverse();

  // First own (non-replayed) node on the active path marks the divergence; the first
  // own user message with text is this branch's first typed prompt.
  let firstPrompt = null, firstNewParent = null, sawNew = false;
  for (const u of path) {
    const n = nodes.get(u);
    if (n.fork) continue;
    if (!sawNew) { sawNew = true; firstNewParent = n.parent; }
    if (!firstPrompt && n.type === 'user' && !n.isMeta && n.content) {
      const cleaned = cleanPrompt(n.content);
      if (cleaned) firstPrompt = cleaned;
    }
  }

  info.title = title;
  // `name` already excludes auto "(Branch N)" names (filtered while scanning).
  info.name = name ? cleanPrompt(name) : null;
  info.named = Boolean(info.name);
  // Fork point: prefer the divergence message in the parent (first own node's parent);
  // fall back to the last replayed message when the fork has no new turns.
  info.forkMsg = info.parent ? (firstNewParent || lastForkMsg) : null;
  // A session can be named from two places: a `custom-title` record (`/rename` on a
  // non-live session, `/branch <name>`) or an `aiTitle` write (Claude's summary, and
  // `/rename` on the live session). Neither kind outranks the other — whichever landed
  // LAST in the transcript is what Claude Code itself displays, so a rename always
  // supersedes an older title and vice versa. Falls back to the first typed prompt.
  info.heading = (info.name && nameAt >= titleAt) ? info.name : (title || info.name);
  info.label = info.heading || firstPrompt || slug || sessionId.slice(0, 8);
  info.strong = Boolean(info.heading); // bold for a name/title, plain for a first prompt
  info.promptFull = (firstPrompt || slug || '').slice(0, 4000);
  return info;
}

// ---------- build + render ----------
function buildForest(nodes) {
  const byId = new Map(nodes.map((n) => [n.sessionId, n]));
  const children = new Map();
  const roots = [];
  for (const n of nodes) {
    const parent = n.parent && byId.has(n.parent) ? n.parent : null;
    n.effectiveParent = parent;
    if (parent) {
      if (!children.has(parent)) children.set(parent, []);
      children.get(parent).push(n);
    } else {
      roots.push(n);
    }
  }
  const byMtime = (a, b) => a.mtime - b.mtime;
  roots.sort(byMtime);
  for (const arr of children.values()) arr.sort(byMtime);
  return { roots, children };
}

function flatten(forest) {
  // DFS assigning stable 1-based indices; record tree-drawing prefix per node.
  const out = [];
  const visit = (node, prefix, isLast, isRoot) => {
    let connector = '';
    if (!isRoot) connector = isLast ? '└─' : '├─';
    out.push({ node, prefix, connector });
    const kids = forest.children.get(node.sessionId) || [];
    const childPrefix = isRoot ? '' : prefix + (isLast ? '   ' : '│  ');
    kids.forEach((k, i) => visit(k, childPrefix, i === kids.length - 1, false));
  };
  forest.roots.forEach((r, i) =>
    visit(r, '', i === forest.roots.length - 1, true));
  out.forEach((row, i) => { row.index = i + 1; });
  return out;
}

// ---------- search ----------
//
// Matching is entirely offline: no network, no transcript re-read. The haystack is
// the text scanSession already kept per node, so a query costs a string scan.

// fzf-style abbreviation matching, but span-limited: the matched characters must
// fall inside a window a few times the needle's own length. Unbounded, a 4-char
// subsequence is findable in almost any sentence ("lcns" hit two thirds of a real
// branch list), which is the difference between a filter and a shrug. Both operands
// must already be lowercased.
function subsequenceMatch(hay, needle) {
  if (!needle) return true;
  const maxSpan = Math.max(needle.length * 3, needle.length + 4);
  const first = needle[0];
  // Try every possible start so a tight match later in the string still counts,
  // rather than only the greedy-from-the-left one.
  for (let start = 0; start <= hay.length - needle.length; start++) {
    if (hay[start] !== first) continue;
    let i = 1;
    let j = start + 1;
    for (; j < hay.length && i < needle.length && j - start < maxSpan; j++) {
      if (hay[j] === needle[i]) i++;
    }
    if (i === needle.length) return true;
  }
  return false;
}

// Two haystacks, because gap-tolerant matching does not survive long text: even
// span-limited, searching a 4000-char prompt (promptFull's cap) that way turns up
// coincidences. Abbreviations are matched only against the SHORT identity fields,
// while the first prompt is searched by literal substring — precise at any length.
//   short: label/heading/name/title, deduped — where "athflw" -> "auth flow" is
//          the point. Deduping matters: label usually equals heading, and a
//          repeated string doubles the room for a coincidental subsequence.
//   full:  short + first prompt + session id — substring only. Session ids live
//          here alone: a subsequence over hex is noise, and an id is pasted exactly.
// Cached off to the side rather than on the node, so nothing leaks into --json.
const hayCache = new WeakMap();
function nodeHaystacks(n) {
  let hay = hayCache.get(n);
  if (hay === undefined) {
    const short = [...new Set([n.label, n.heading, n.name, n.title].filter(Boolean))]
      .join('   ').toLowerCase();
    const full = [short, n.promptFull || '', n.sessionId].join('   ').toLowerCase();
    hay = { short, full };
    hayCache.set(n, hay);
  }
  return hay;
}

// Every token must hit somewhere, so word order doesn't matter: "auth flow" finds
// "flow for authentication".
function matchNode(n, tokens) {
  const hay = nodeHaystacks(n);
  return tokens.every((t) => hay.full.includes(t) || subsequenceMatch(hay.short, t));
}

// Rows matching `query`, plus each match's ancestors so the tree still reads as a
// tree. The pruned set is re-flattened for correct connectors/prefixes, then the
// original 1-based indices are restored — they line up with the `branch-graph <n>`
// CLI arg, so rows must keep their real numbers rather than being renumbered.
// Each returned row carries `match`: true for a real hit, false for a row kept only
// as context. Rows in the unfiltered list have no `match` property at all, so
// `match !== false` reads as "not a context row" in both cases.
function filterRows(allRows, query) {
  const tokens = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!tokens.length) return allRows;
  const byId = new Map(allRows.map((r) => [r.node.sessionId, r.node]));
  const matched = new Set();
  for (const r of allRows) {
    if (matchNode(r.node, tokens)) matched.add(r.node.sessionId);
  }
  if (matched.size === 0) return [];
  const keep = new Set();
  for (const id of matched) {
    // Walk up so a matched leaf keeps the branch it hangs off. Stopping at an
    // already-kept node is safe: we always add a whole chain at once.
    for (let n = byId.get(id); n && !keep.has(n.sessionId); n = byId.get(n.effectiveParent)) {
      keep.add(n.sessionId);
    }
  }
  // Ancestors of every match are kept too, so buildForest recomputes exactly the
  // effectiveParent values it already set — the subset can never orphan a node.
  // Dropping that invariant would silently break p/← parent navigation.
  const rows = flatten(buildForest(allRows
    .filter((r) => keep.has(r.node.sessionId))
    .map((r) => r.node)));
  const origIndex = new Map(allRows.map((r) => [r.node.sessionId, r.index]));
  for (const r of rows) {
    r.index = origIndex.get(r.node.sessionId);
    r.match = matched.has(r.node.sessionId);
  }
  return rows;
}

function resumeLine(id) { return `/resume ${id}`; }

// Hand the terminal over to a Claude session on `sessionId`. Never returns. Callers
// holding terminal state (the picker) must restore() before calling.
function launchResume(sessionId) {
  const { spawnSync } = require('child_process');
  const res = spawnSync('claude', ['-r', sessionId], { stdio: 'inherit' });
  if (res.error) {
    // `claude` missing or not executable — fall back to the paste-able line rather
    // than exiting silently as if the switch had happened.
    process.stderr.write(`branch-graph: could not run \`claude\` ` +
      `(${res.error.code || res.error.message})\n`);
    console.log(resumeLine(sessionId));
    process.exit(1);
  }
  process.exit(res.status == null ? 0 : res.status);
}

function renderList(rows, projectName, meta) {
  const idxW = String(rows.length).length;
  // label column width for alignment
  const treeStrings = rows.map((r) => `${r.prefix}${r.connector}●`);
  const treeW = Math.max(...treeStrings.map((s) => s.length));
  const labelW = Math.min(40, Math.max(...rows.map((r) => truncate(r.node.label, 40).length)));

  const lines = [];
  lines.push(bold(`Branches in ${projectName}:`));
  for (const r of rows) {
    const n = r.node;
    const idx = String(r.index).padStart(idxW);
    const glyph = n.current ? green('●') : n.latest ? yellow('●') : '●';
    const highlighted = n.current || n.latest;
    const tree = `${r.prefix}${r.connector}${glyph}`.padEnd(
      treeW + (useColor && highlighted ? 9 : 0));
    const sid = cyan(n.sessionId.slice(0, 8));
    // A title or name is shown bold; a first prompt is shown as plain text.
    const plain = truncate(n.label, 40).padEnd(labelW);
    const label = n.strong ? bold(plain) : plain;
    let action;
    if (n.current) action = yellow('← current (this session)');
    else {
      action = dim(resumeLine(n.sessionId));
      if (n.latest) action += ' ' + yellow('(most recent)');
    }
    lines.push(`  ${idx}  ${tree} ${sid}  ${label}  ${action}`);
  }
  lines.push('');
  if (meta && meta.elsewhere) {
    lines.push(dim(`Current session ${meta.currentId.slice(0, 8)} belongs to another project; ` +
      `marking the most recent here.`));
  }
  lines.push(dim('Paste a /resume line to switch in place. Or run `!branch-graph <n>` for one line.'));
  return lines.join('\n');
}

// ---------- mouse diagnostic ----------
// `branch-graph --debug-mouse`: mirrors the picker's setup EXACTLY (alternate screen
// + any-motion tracking) and counts motion events. Move the pointer inside WITHOUT
// clicking or re-entering the window. On exit it reports how many motion events it
// saw — if 0 until you move out and back in, the terminal suppresses motion on the
// alternate screen (the iTerm2 quirk we're chasing); if it counts up immediately,
// the terminal is fine and the bug is elsewhere.
function debugMouse() {
  const out = process.stdout, inp = process.stdin;
  if (!out.isTTY || !inp.isTTY) {
    process.stderr.write('branch-graph --debug-mouse: needs a real terminal ' +
      '(run it directly in your shell, not via Claude Code `!`).\n');
    process.exit(2);
  }
  const w = (s) => out.write(s);
  let count = 0, last = 'none', done = false;
  function finish() {
    if (done) return; done = true;
    try { inp.setRawMode(false); } catch { /* ignore */ }
    w('\x1b[?1003l\x1b[?1006l'); // mouse off
    w('\x1b[?25h\x1b[?1049l');   // cursor on, leave alt screen
    out.write(`\nSaw ${count} motion event(s) on the alternate screen. last: ${last}\n` +
      (count === 0
        ? 'iTerm2 quirk CONFIRMED: motion is suppressed on the alt screen until re-entry.\n'
        : 'Alt screen reports motion fine — the picker bug is elsewhere.\n'));
    process.exit(0);
  }
  function paint() {
    let b = '\x1b[H\x1b[2J';
    b += 'Mouse debug — alt-screen, mirrors the picker.\r\n';
    b += 'MOVE inside WITHOUT clicking or re-entering the window. Press q to quit.\r\n\r\n';
    b += `motion events: ${count}\r\n`;
    b += `last: ${last}\r\n`;
    if (count === 0) {
      b += '\r\n(still 0 — keep moving inside. If it only counts up after you move out\r\n';
      b += ' and back in, that confirms the alt-screen motion-onset quirk.)\r\n';
    }
    w(b);
  }
  // Same init order/sequences as runInteractive().
  inp.setRawMode(true);
  inp.setEncoding('utf8');
  w('\x1b[?1049h\x1b[?25l');       // alt screen + hide cursor
  w('\x1b[?1003h\x1b[?1006h');     // any-motion + SGR coords
  paint();
  inp.on('data', (d) => {
    if (d.includes('q') || d.includes('\x03')) return finish();
    const m = /\x1b\[<(\d+);(\d+);(\d+)([Mm])/.exec(d);
    if (m && (+m[1] & 32)) { count++; last = `b=${m[1]} x=${m[2]} y=${m[3]} ${m[4]}`; paint(); }
  });
  inp.resume();
  process.on('SIGINT', finish);
  process.on('exit', () => { if (!done) { try { inp.setRawMode(false); } catch {} w('\x1b[?1003l\x1b[?1006l\x1b[?25h\x1b[?1049l'); } });
}

// ---------- interactive TUI (standalone terminal only, zero deps) ----------
function wrapText(s, width) {
  const out = [];
  let line = '';
  for (const w of String(s).split(/\s+/).filter(Boolean)) {
    let word = w;
    while (word.length > width) { // hard-break very long words
      if (line) { out.push(line); line = ''; }
      out.push(word.slice(0, width));
      word = word.slice(width);
    }
    if (!line) line = word;
    else if ((line + ' ' + word).length <= width) line += ' ' + word;
    else { out.push(line); line = word; }
  }
  if (line) out.push(line);
  return out;
}

function formatTime(ms) {
  try { return new Date(ms).toLocaleString(); } catch { return ''; }
}

// Short last-active stamp for the picker's tag column: `15/08  1:05PM`. Built by hand
// rather than toLocaleString so the format and width are identical on every machine and
// locale. Always 13 chars wide (hour space-padded) so the right-aligned tags line up.
function shortTime(ms) {
  if (!ms) return '';
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return '';
  const p2 = (n) => String(n).padStart(2, '0');
  const h24 = d.getHours();
  const h = h24 % 12 || 12;
  return `${p2(d.getDate())}/${p2(d.getMonth() + 1)} ` +
    `${h}:${p2(d.getMinutes())}${h24 < 12 ? 'AM' : 'PM'}`;
}

function runInteractive(allRows, ctx) {
  const out = process.stdout;
  const inp = process.stdin;
  // Width of the index column stays keyed to the FULL tree so it doesn't jitter as
  // rows are filtered away while typing.
  const idxW = String(allRows.length).length;
  let rows = allRows;   // current view: the whole tree, or the search-filtered subset
  let selected = rows.findIndex((r) => r.node.current || r.node.latest);
  if (selected < 0) selected = 0;
  let scrollTop = 0;
  let rowYMap = []; // 1-based screen line -> row index, for mouse hit-testing
  let searchMode = false; // true while typing a query
  let query = '';
  // Both maps hold ROW INDICES, so they are invalid the moment `rows` changes —
  // setView() rebuilds them together with the view.
  let rowBySessionId = new Map();
  let childRows = new Map();
  function reindex() {
    rowBySessionId = new Map(rows.map((r, i) => [r.node.sessionId, i]));
    // Row indices of each node's children. flatten() visits children in the mtime order
    // buildForest sorted them into, so the last entry is always the most recent child.
    childRows = new Map();
    rows.forEach((r, i) => {
      const p = r.node.effectiveParent;
      if (!p) return;
      const a = childRows.get(p);
      if (a) a.push(i); else childRows.set(p, [i]);
    });
  }
  reindex();

  // Swap in a new view. The highlight stays put only if that branch is still a real
  // match — otherwise it lands on the first hit. Falling back to row 0 instead put
  // the selection on the topmost context ancestor, which reads as the search having
  // picked the wrong branch.
  function setView(next) {
    const keepId = rows.length ? rows[selected].node.sessionId : null;
    rows = next;
    reindex();
    const at = keepId != null ? rowBySessionId.get(keepId) : undefined;
    if (at != null && rows[at].match !== false) selected = at;
    else {
      const firstHit = rows.findIndex((r) => r.match !== false);
      selected = firstHit >= 0 ? firstHit : 0;
    }
    scrollTop = 0;
  }
  function applyQuery(q) {
    query = q;
    setView(filterRows(allRows, query));
    render();
  }
  // clear:true also drops the filter; otherwise just leaves typing mode.
  function exitSearch(clear) {
    searchMode = false;
    if (clear && query) applyQuery('');
    else render();
  }

  let inputBuf = ''; // carries a partial escape sequence between data events
  let escTimer = null;
  const w = (s) => out.write(s);

  let restored = false;
  function restore() {
    if (restored) return;
    restored = true;
    if (escTimer) { clearTimeout(escTimer); escTimer = null; }
    try { if (inp.setRawMode) inp.setRawMode(false); } catch { /* ignore */ }
    w('\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l'); // mouse off
    w('\x1b[2J\x1b[H'); // clear our full-screen UI from the main buffer
    w('\x1b[?25h');     // cursor on
    try { inp.pause(); } catch { /* ignore */ }
  }
  function quit(code) { restore(); process.exit(code); }
  function resume(node) {
    restore();
    launchResume(node.sessionId);
  }
  // A search can filter every row away, so nothing may read rows[selected] blindly.
  function current() { return rows.length ? rows[selected] : null; }
  function move(delta) {
    if (!rows.length) return;
    selected = Math.max(0, Math.min(rows.length - 1, selected + delta));
    render();
  }
  function jumpTo(i) {
    if (!rows.length) return;
    selected = Math.max(0, Math.min(rows.length - 1, i));
    render();
  }
  // Jump to the branch this one forked from. effectiveParent is non-null only when that
  // session is in this project, so it always resolves to a row; roots and forks whose
  // parent lives elsewhere have nowhere to go.
  function selectParent() {
    const row = current();
    if (!row) return;
    const pid = row.node.effectiveParent;
    if (!pid) return;
    const i = rowBySessionId.get(pid);
    if (i == null || i === selected) return;
    selected = i;
    render();
  }
  // Descend to this branch's most recent child; leaves have nowhere to go.
  function selectChild() {
    const row = current();
    if (!row) return;
    const kids = childRows.get(row.node.sessionId);
    if (!kids) return;
    selected = kids[kids.length - 1];
    render();
  }

  function rowText(r, cols, isSel) {
    const n = r.node;
    const idx = String(r.index).padStart(idxW);
    const sid = n.sessionId.slice(0, 8);
    // Tag trailing the label: the current/most-recent marker when the row has one, else
    // a short last-active stamp so every row says when it was last touched.
    const stamp = shortTime(n.mtime);
    const tag = n.current ? '  ← current' : n.latest ? '  (most recent)'
      : (stamp ? '  ' + stamp : '');
    const fixed = `${idx}  ${r.prefix}${r.connector}● ${sid}  `;
    const room = Math.max(8, cols - fixed.length - tag.length);
    const label = truncate(n.label, room);
    let text = `${fixed}${label}${tag}`;
    if (text.length > cols) text = text.slice(0, cols);
    if (isSel) return '\x1b[7m' + text.padEnd(cols) + '\x1b[0m';
    // A search context row — kept only to show where a match hangs off, not a hit
    // itself. Dim the whole line so the actual matches stand out; the usual per-part
    // coloring would just compete for attention.
    if (r.match === false) return dim(text);
    // non-selected: colorize glyph + id; bold a title/name, plain a first prompt
    const glyph = n.current ? green('●') : n.latest ? yellow('●') : '●';
    const shownLabel = n.strong ? bold(label) : label;
    return `${idx}  ${r.prefix}${r.connector}${glyph} ${cyan(sid)}  ${shownLabel}` +
      (tag ? dim(tag) : '');
  }

  function buildDetail(node, cols) {
    const width = Math.min(cols, 100);
    const lines = [];
    lines.push(dim('─'.repeat(Math.min(cols, 80))));
    const parent = node.effectiveParent
      ? `forked from ${node.effectiveParent.slice(0, 8)}` +
        (node.forkMsg ? ` @ ${node.forkMsg.slice(0, 8)}` : '')
      : 'root session';
    lines.push(bold(node.sessionId));
    lines.push(dim(`${parent} · last active ${formatTime(node.mtime)}`));
    lines.push('');
    // Heading: the winning name/title computed in scanSession (a rename supersedes the
    // generated title rather than sitting alongside it). Body is always the first
    // prompt, so a titled/named branch shows both.
    const heading = node.heading;
    if (heading) lines.push(bold(truncate(heading, width)));
    const body = node.promptFull || dim('(no prompt text)');
    const wrapped = wrapText(body, width);
    const maxLines = 6;
    for (const wl of wrapped.slice(0, maxLines)) lines.push(wl);
    if (wrapped.length > maxLines) lines.push(dim('…'));
    return lines;
  }

  function render() {
    const cols = out.columns || 80;
    const term = out.rows || 24;
    const row = current();
    // With no matches there is no branch to describe, so the detail panel collapses
    // to a single hint line rather than reading rows[selected] off an empty view.
    const detail = row ? buildDetail(row.node, cols)
      : [dim('─'.repeat(Math.min(cols, 80))), dim('no branch matches this search')];
    const searchLine = searchMode || query;      // shown between header and rows
    const chromeTop = 2 + (searchLine ? 1 : 0);  // header + [search] + blank
    const chromeBottom = 1 + detail.length + 1 + 1; // blank + detail + blank + footer
    let viewport = term - chromeTop - chromeBottom;
    if (viewport < 3) viewport = 3;
    if (selected < scrollTop) scrollTop = selected;
    if (selected >= scrollTop + viewport) scrollTop = selected - viewport + 1;
    if (scrollTop < 0) scrollTop = 0;
    const end = Math.min(rows.length, scrollTop + viewport);

    // Build the whole frame, then write once. Home the cursor and clear each line
    // (\x1b[K) instead of clearing the whole screen — no flicker as hover repaints.
    let buf = '\x1b[H';
    const put = (s) => { buf += s + '\x1b[K\r\n'; };
    put(bold(`Branches in ${ctx.projectName}`) +
      (rows.length > viewport ? dim(`  (${selected + 1}/${rows.length})`) : ''));
    if (searchLine) {
      // Count hits, not rows: the view also carries dimmed context ancestors, and
      // reporting those as matches overstates what was found.
      const hits = rows.reduce((k, r) => k + (r.match === false ? 0 : 1), 0);
      const count = !query ? ''
        : hits === 0 ? '  no matches'
          : `  ${hits} of ${allRows.length}`;
      // A block cursor while typing, so it's obvious the keyboard is going here.
      put(cyan('search: ') + query + (searchMode ? '\x1b[7m \x1b[0m' : '') + dim(count));
    }
    put('');
    rowYMap = [];
    let y = chromeTop + 1;
    for (let i = scrollTop; i < end; i++) {
      rowYMap[y] = i;
      put(rowText(rows[i], cols, i === selected));
      y++;
    }
    for (let i = end; i < scrollTop + viewport; i++) put('');
    put('');
    for (const dl of detail) put(dl);
    put('');
    if (ctx.elsewhere) {
      put(dim(`current session ${ctx.currentId.slice(0, 8)} is in another project`));
    }
    buf += dim(searchMode
      ? 'type to filter   ↑/↓: select   Enter: accept   Esc: cancel'
      : query
        ? '↑/↓/hover: navigate   p/←: parent   →: child   Enter/click: resume   Esc: clear search'
        : '↑/↓/hover: navigate   s: search   p/←: parent   →: child   Enter/click: resume   Esc: quit'
    ) + '\x1b[K';
    buf += '\x1b[J'; // clear anything left below from a previous taller frame
    w(buf);
  }

  function handleMouse(b, x, yy, type) {
    if (b === 64 || b === 65) return; // ignore wheel — scrolling must not change selection
    const row = rowYMap[yy];
    if (row == null) return;          // not over a branch row
    const isMotion = (b & 32) !== 0;
    if (isMotion) { // hover → select the row under the cursor
      if (row !== selected) { selected = row; render(); }
      return;
    }
    if (type === 'M' && (b & 3) === 0) { // left-click → resume that branch
      selected = row;
      resume(rows[selected].node);
    }
  }

  // A lone ESC that never grows into a sequence within the timeout = the Escape key.
  // Escape unwinds one layer at a time: typing → filter → quit. Only a bare picker
  // with no search in play exits, so a search can never be lost to a stray keypress.
  function flushPending() {
    escTimer = null;
    const p = inputBuf; inputBuf = '';
    if (p !== '\x1b') return; // stalled/garbled partial sequence: drop it
    if (searchMode) exitSearch(true);
    else if (query) applyQuery('');
    else quit(0);
  }
  function armFlush() {
    if (escTimer) clearTimeout(escTimer);
    escTimer = setTimeout(flushPending, 50);
  }

  // Terminal input can split an escape sequence across data events — in any-motion
  // mouse mode (1003), motion floods events, so mid-sequence splits are common. The
  // old parser mis-handled a split: it advanced past the stray ESC and desynced the
  // stream, so hover/clicks did nothing until the next event happened to resync (and
  // a split right at the ESC byte could even quit). We now buffer a partial sequence
  // and re-join it with the next chunk, and parse CSI/SS3 generically so unknown or
  // incomplete sequences are never mistaken for the Escape key.
  function onData(data) {
    if (escTimer) { clearTimeout(escTimer); escTimer = null; }
    const buf = inputBuf + data;
    inputBuf = '';
    let i = 0;
    while (i < buf.length) {
      const ch = buf[i];
      if (ch !== '\x1b') {
        // Search mode swallows every printable key, so j/k/p/q/g type instead of
        // navigating. Arrows (handled below) still move the selection while typing.
        if (searchMode) {
          if (ch === '\x03') { quit(0); return; }              // Ctrl-C always quits
          // Accept, keeping the filter — but never onto an empty view, which would
          // strand the picker with nothing to select. Keep typing instead.
          else if (ch === '\r' || ch === '\n') { if (rows.length) exitSearch(false); }
          else if (ch === '\x7f' || ch === '\b') applyQuery(query.slice(0, -1));
          else if (ch === '\x15') applyQuery('');              // Ctrl-U: clear query
          else if (ch >= ' ') applyQuery(query + ch);          // ignore other controls
          i++; continue;
        }
        if (ch === '\r' || ch === '\n') {
          const row = current();
          if (row) { resume(row.node); return; }
        } else if (ch === 's' || ch === '/') { searchMode = true; render(); }
        else if (ch === 'k') move(-1);
        else if (ch === 'j') move(1);
        else if (ch === 'p') selectParent();
        else if (ch === 'g') jumpTo(0);
        else if (ch === 'G') jumpTo(rows.length - 1);
        else if (ch === 'q' || ch === '\x03') { quit(0); return; }
        i++; continue;
      }
      const rest = buf.slice(i);
      if (rest === '\x1b') { inputBuf = rest; armFlush(); return; } // ESC key or seq start
      if (rest[1] === '[') { // CSI: params [0x30-3f], intermediates [0x20-2f], final [0x40-7e]
        const csi = /^\x1b\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]/.exec(rest);
        if (!csi) { inputBuf = rest; armFlush(); return; } // incomplete → await more bytes
        const seq = csi[0];
        const mm = /^\x1b\[<(\d+);(\d+);(\d+)([Mm])$/.exec(seq);
        if (mm) handleMouse(+mm[1], +mm[2], +mm[3], mm[4]);
        else {
          const f = seq[seq.length - 1];
          if (f === 'A') move(-1);
          else if (f === 'B') move(1);
          else if (f === 'H') jumpTo(0);
          else if (f === 'F') jumpTo(rows.length - 1);
          else if (f === 'D') selectParent();
          else if (f === 'C') selectChild();
          // any other CSI (focus events, etc.): ignore rather than quit
        }
        i += seq.length; continue;
      }
      if (rest[1] === 'O') { // SS3 application cursor keys: ESC O <final>
        if (rest.length < 3) { inputBuf = rest; armFlush(); return; }
        const f = rest[2];
        if (f === 'A') move(-1);
        else if (f === 'B') move(1);
        else if (f === 'H') jumpTo(0);
        else if (f === 'F') jumpTo(rows.length - 1);
        else if (f === 'D') selectParent();
        else if (f === 'C') selectChild();
        i += 3; continue;
      }
      // ESC + other byte (Alt-combo, etc.). Mid-typing that must not kill the picker
      // and lose the query, so swallow it; outside search mode it quits, as before.
      if (searchMode) { i += 2; continue; }
      quit(0); return;
    }
  }

  inp.setRawMode(true);
  inp.setEncoding('utf8');
  // We render full-screen on the MAIN buffer, not the alternate screen (1049). iTerm2
  // suppresses any-motion mouse reporting while the alt screen is active until the
  // pointer re-enters the window, so hover felt dead when the cursor started inside;
  // the main buffer reports motion immediately. render() uses absolute positioning
  // (\x1b[H + \x1b[J), so we just hide the cursor and clear the screen ourselves.
  w('\x1b[?25l');               // hide cursor
  // Enable any-motion mouse tracking (1003 also reports button press/release) with
  // SGR coordinates (1006). We deliberately do NOT set 1000 as well: 1000 and 1003
  // are alternate tracking modes and setting 1000 first can leave terminals in
  // press-only state.
  w('\x1b[?1003h\x1b[?1006h');
  w('\x1b[2J\x1b[H');           // clear the visible screen, cursor home
  render();
  // Attach the data listener BEFORE resume(): once stdin is flowing with no listener,
  // Node discards input, so early motion events (pointer already inside) were lost.
  inp.on('data', onData);
  inp.resume();
  out.on('resize', render);
  process.on('SIGINT', () => quit(130));
  process.on('SIGTERM', () => quit(143));
  process.on('exit', restore);
  process.on('uncaughtException', (e) => { restore(); console.error(e); process.exit(1); });
}

function help() {
  return [
    'branch-graph — visualize Claude Code session fork tree (zero tokens)',
    '',
    'Usage:',
    '  !branch-graph            list the fork tree with a /resume line per branch',
    '  !branch-graph <n>        print only the /resume line for branch number <n>',
    '  !branch-graph ..         go up one fork level from the most recent branch',
    '',
    'In a real terminal it opens an interactive picker (↑/↓ or mouse to navigate, p/← to',
    'jump to the parent branch and → to its most recent child, Enter/click to resume that',
    'branch). Piped or inside Claude Code it prints a list.',
    '',
    'Search (picker only):',
    '  s or /             start typing a query; the tree filters as you type, keeping',
    '                     each match\'s parent branches greyed out for context. The',
    '                     selection lands on the first real match.',
    '  Enter              accept the filter and go back to navigating it',
    '  Esc                cancel typing / clear the filter / quit (one layer per press)',
    '',
    '  Matching runs locally over branch names, titles, first prompts and session ids',
    '  (not full transcript bodies). It is case-insensitive and word order does not',
    '  matter, so "auth flow" finds "flow for authentication". Against names and titles',
    '  you can also skip letters — "lcns" finds "Choose license" — while first prompts',
    '  match on text you actually typed.',
    '',
    'Flags:',
    '  .., -p, --parent   jump to the parent of the most recent branch: launches',
    '                     `claude -r` in a terminal, prints /resume when piped or',
    '                     inside Claude Code. Takes precedence over the picker.',
    '  -i, --interactive  force the interactive picker (requires a terminal)',
    '  --no-interactive   force plain list output',
    '  --project <path>   a working dir or ~/.claude/projects/<dir> to inspect',
    '  --json             machine-readable output',
    '  --list             force list output',
    '  --color/--no-color force or disable ANSI color (auto-on in a terminal & Claude Code)',
    '  -h, --help         show this help',
  ].join('\n');
}

// ---------- main ----------
(async function main() {
  const opts = parseArgs(process.argv.slice(2));
  useColor = decideColor(opts);
  if (opts.help) { console.log(help()); return; }
  if (opts.debugMouse) { debugMouse(); return; }

  const dir = resolveProjectDir(opts.project);
  if (!fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) {
    process.stderr.write(`branch-graph: no sessions found for this project\n  (looked in: ${dir})\n`);
    process.exit(1);
  }

  const files = fs.readdirSync(dir)
    .filter((f) => f.endsWith('.jsonl'))
    .map((f) => path.join(dir, f))
    .filter((f) => { try { return fs.statSync(f).isFile(); } catch { return false; } });

  if (files.length === 0) {
    process.stderr.write(`branch-graph: no session transcripts in ${dir}\n`);
    process.exit(1);
  }

  const currentId = process.env.CLAUDE_CODE_SESSION_ID || null;
  let newestFile = null, newestMtime = -1;

  const nodes = [];
  for (const file of files) {
    const sessionId = path.basename(file, '.jsonl');
    let mtime = 0;
    try { mtime = fs.statSync(file).mtimeMs; } catch {}
    if (mtime > newestMtime) { newestMtime = mtime; newestFile = sessionId; }
    const info = await scanSession(file, sessionId);
    info.mtime = mtime;
    nodes.push(info);
  }

  // "current" = the session we're running inside, but only if it belongs to the
  // project being shown (CLAUDE_CODE_SESSION_ID matches a file here). Otherwise we
  // can't truthfully claim one is "this session" — fall back to flagging the most
  // recently written session as "most recent" so there's always a useful anchor.
  const exactMatch = currentId && nodes.some((n) => n.sessionId === currentId);
  const elsewhere = currentId && !exactMatch; // current session is in another project
  for (const n of nodes) {
    n.current = exactMatch && n.sessionId === currentId;
    n.latest = !exactMatch && n.sessionId === newestFile;
  }

  const forest = buildForest(nodes);
  const rows = flatten(forest);
  const projectName = opts.project
    ? path.basename(path.resolve(opts.project))
    : path.basename(process.cwd());

  if (opts.json) {
    console.log(JSON.stringify(rows.map((r) => ({
      index: r.index,
      sessionId: r.node.sessionId,
      parent: r.node.effectiveParent,
      forkMessageUuid: r.node.forkMsg,
      label: r.node.label,
      name: r.node.name,
      named: r.node.named,
      title: r.node.title,
      firstPrompt: r.node.promptFull || null,
      current: r.node.current,
      latest: Boolean(r.node.latest),
      resume: r.node.current ? null : resumeLine(r.node.sessionId),
    })), null, 2));
    return;
  }

  if (opts.index != null) {
    const row = rows.find((r) => r.index === opts.index);
    if (!row) {
      process.stderr.write(`branch-graph: no branch #${opts.index} (valid: 1-${rows.length})\n`);
      process.exit(1);
    }
    if (row.node.current) {
      console.log(`Branch #${opts.index} is the current session — already here.`);
      return;
    }
    console.log(resumeLine(row.node.sessionId));
    console.log(dim(`(other terminal: claude -r ${row.node.sessionId})`));
    return;
  }

  // A real terminal on both ends is what lets us hand the terminal over — to the picker
  // below, or straight to a `claude -r` for `..`.
  const canInteract = Boolean(process.stdout.isTTY && process.stdin.isTTY);

  // `..` / -p: go up one fork level. The anchor is the session we're running inside when
  // it belongs to this project, else the most recently written one — the same precedence
  // as the current/latest markers above, so `..` means "up from where I just was".
  if (opts.parent) {
    const anchorId = exactMatch ? currentId : newestFile;
    const anchor = nodes.find((n) => n.sessionId === anchorId);
    if (!anchor) {
      process.stderr.write('branch-graph: could not determine the most recent branch\n');
      process.exit(1);
    }
    // buildForest() nulls effectiveParent both for a true root and for a fork whose
    // parent transcript lives elsewhere; anchor.parent is what tells them apart.
    if (!anchor.effectiveParent) {
      if (anchor.parent) {
        process.stderr.write(`branch-graph: parent ${anchor.parent.slice(0, 8)} of ` +
          `${anchorId.slice(0, 8)} has no transcript in this project\n`);
      } else {
        process.stderr.write(`branch-graph: most recent branch ${anchorId.slice(0, 8)} ` +
          `(${truncate(anchor.label, 40)}) is a root — no parent branch\n`);
      }
      process.exit(1);
    }
    const parentId = anchor.effectiveParent;
    // Launch only where we can own the terminal. Inside Claude Code's piped `!` a nested
    // `claude` would be wrong, so print the paste-able line instead.
    const canLaunch = canInteract && !process.env.CLAUDECODE &&
      opts.interactive !== false && !opts.list;
    if (canLaunch) launchResume(parentId); // never returns
    console.log(resumeLine(parentId));
    console.log(dim(`(other terminal: claude -r ${parentId})`));
    return;
  }

  // Interactive TUI: only in a real terminal, never inside Claude Code's piped `!`.
  if (opts.interactive === true && !canInteract) {
    process.stderr.write('branch-graph: --interactive requires a terminal (TTY)\n');
    process.exit(2);
  }
  const wantInteractive = canInteract && (
    opts.interactive === true ||
    (opts.interactive !== false && !process.env.CLAUDECODE && !opts.list)
  );
  if (wantInteractive) {
    runInteractive(rows, { projectName, elsewhere, currentId });
    return; // runInteractive hands off the terminal (resume) or exits on quit
  }

  console.log(renderList(rows, projectName, { elsewhere, currentId }));
})().catch((err) => {
  process.stderr.write(`branch-graph: ${err && err.stack ? err.stack : err}\n`);
  process.exit(1);
});
