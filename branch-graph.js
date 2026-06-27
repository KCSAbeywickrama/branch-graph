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
// Flags:
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
    color: null, interactive: null, debugMouse: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '-h' || a === '--help') opts.help = true;
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
    name: null, named: false, title: null, promptFull: '' };
  let title = null, slug = null, name = null;
  let lastForkMsg = null;        // messageUuid of the last replayed line = divergence leaf
  let leafUuid = null, lastUuid = null;
  const nodes = new Map();       // uuid -> { parent, type, isMeta, fork, content }
  const rl = readline.createInterface({
    input: fs.createReadStream(file, { encoding: 'utf8' }),
    crlfDelay: Infinity,
  });
  for await (const line of rl) {
    if (!line) continue;
    let o;
    try { o = JSON.parse(line); } catch { continue; }
    if (o.type === 'last-prompt' && o.leafUuid) leafUuid = o.leafUuid; // live head pointer
    if (o.forkedFrom && o.forkedFrom.sessionId) {
      if (!info.parent) info.parent = o.forkedFrom.sessionId;
      if (o.forkedFrom.messageUuid) lastForkMsg = o.forkedFrom.messageUuid;
    }
    if (typeof o.aiTitle === 'string' && o.aiTitle.trim()) title = o.aiTitle.trim();
    else if (o.type === 'ai-title') {
      const t = o.title || o.aiTitle || o.content;
      if (typeof t === 'string' && t.trim()) title = t.trim();
    }
    // User-set display name (the one shown in /resume). Last write wins (renames).
    if (typeof o.customTitle === 'string' && o.customTitle.trim()) name = o.customTitle.trim();
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
  // Only an explicit `/branch myname` counts as a name; auto "(Branch N)" is ignored.
  info.name = (name && !AUTO_NAME_RE.test(name.trim())) ? cleanPrompt(name) : null;
  info.named = Boolean(info.name);
  // Fork point: prefer the divergence message in the parent (first own node's parent);
  // fall back to the last replayed message when the fork has no new turns.
  info.forkMsg = info.parent ? (firstNewParent || lastForkMsg) : null;
  // Label precedence: aiTitle → user name → first prompt. Titles and names are a
  // descriptive label (rendered bold); the first prompt is shown as plain text.
  info.label = title || info.name || firstPrompt || slug || sessionId.slice(0, 8);
  info.strong = Boolean(title || info.name); // bold when the label is a title or name
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

function resumeLine(id) { return `/resume ${id}`; }

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

function runInteractive(rows, ctx) {
  const out = process.stdout;
  const inp = process.stdin;
  const idxW = String(rows.length).length;
  let selected = rows.findIndex((r) => r.node.current || r.node.latest);
  if (selected < 0) selected = 0;
  let scrollTop = 0;
  let rowYMap = []; // 1-based screen line -> row index, for mouse hit-testing
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
    const { spawnSync } = require('child_process');
    const res = spawnSync('claude', ['-r', node.sessionId], { stdio: 'inherit' });
    process.exit(res.status == null ? 0 : res.status);
  }
  function move(delta) {
    selected = Math.max(0, Math.min(rows.length - 1, selected + delta));
    render();
  }

  function rowText(r, cols, isSel) {
    const n = r.node;
    const idx = String(r.index).padStart(idxW);
    const sid = n.sessionId.slice(0, 8);
    const tag = n.current ? '  ← current' : n.latest ? '  (most recent)' : '';
    const fixed = `${idx}  ${r.prefix}${r.connector}● ${sid}  `;
    const room = Math.max(8, cols - fixed.length - tag.length);
    const label = truncate(n.label, room);
    let text = `${fixed}${label}${tag}`;
    if (text.length > cols) text = text.slice(0, cols);
    if (isSel) return '\x1b[7m' + text.padEnd(cols) + '\x1b[0m';
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
    // Heading: aiTitle, else the user name (same precedence as the label). Body is
    // always the first prompt, so a titled/named branch shows both.
    const heading = node.title || node.name;
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
    const node = rows[selected].node;
    const detail = buildDetail(node, cols);
    const chromeTop = 2;                    // header + blank
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
    put('');
    rowYMap = [];
    let y = 3;
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
    buf += dim('↑/↓ or hover: navigate   Enter/click: resume   Esc: quit') + '\x1b[K';
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
  function flushPending() {
    escTimer = null;
    const p = inputBuf; inputBuf = '';
    if (p === '\x1b') quit(0); // Escape key → quit
    // otherwise a stalled/garbled partial sequence: drop it
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
        if (ch === '\r' || ch === '\n') { resume(rows[selected].node); return; }
        else if (ch === 'k') move(-1);
        else if (ch === 'j') move(1);
        else if (ch === 'g') { selected = 0; render(); }
        else if (ch === 'G') { selected = rows.length - 1; render(); }
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
          else if (f === 'H') { selected = 0; render(); }
          else if (f === 'F') { selected = rows.length - 1; render(); }
          // any other CSI (focus events, etc.): ignore rather than quit
        }
        i += seq.length; continue;
      }
      if (rest[1] === 'O') { // SS3 application cursor keys: ESC O <final>
        if (rest.length < 3) { inputBuf = rest; armFlush(); return; }
        const f = rest[2];
        if (f === 'A') move(-1);
        else if (f === 'B') move(1);
        else if (f === 'H') { selected = 0; render(); }
        else if (f === 'F') { selected = rows.length - 1; render(); }
        i += 3; continue;
      }
      quit(0); return; // ESC + other byte (Alt-combo, etc.) → quit, as before
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
    '',
    'In a real terminal it opens an interactive picker (↑/↓ or mouse to navigate,',
    'Enter/click to resume that branch). Piped or inside Claude Code it prints a list.',
    '',
    'Flags:',
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

  // Interactive TUI: only in a real terminal, never inside Claude Code's piped `!`.
  const canInteract = Boolean(process.stdout.isTTY && process.stdin.isTTY);
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
