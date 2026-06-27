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
    color: null, interactive: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '-h' || a === '--help') opts.help = true;
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
    .replace(/<command-[^>]*>[\s\S]*?<\/command-[^>]*>/g, '')
    .replace(/<local-command-[^>]*>[\s\S]*?<\/local-command-[^>]*>/g, '')
    .replace(/<[^>]+>/g, '')
    .replace(/\s+/g, ' ')
    .trim();
}
function truncate(s, n) {
  return s.length > n ? s.slice(0, n - 1) + '…' : s;
}

// ---------- scan one session file ----------
async function scanSession(file, sessionId) {
  const info = { sessionId, parent: null, forkMsg: null, label: null,
    title: null, promptFull: '' };
  let title = null, slug = null, firstPrompt = null;
  const rl = readline.createInterface({
    input: fs.createReadStream(file, { encoding: 'utf8' }),
    crlfDelay: Infinity,
  });
  for await (const line of rl) {
    if (!line) continue;
    let o;
    try { o = JSON.parse(line); } catch { continue; }
    if (!info.parent && o.forkedFrom && o.forkedFrom.sessionId) {
      info.parent = o.forkedFrom.sessionId;
      info.forkMsg = o.forkedFrom.messageUuid || null;
    }
    if (typeof o.aiTitle === 'string' && o.aiTitle.trim()) title = o.aiTitle.trim();
    else if (o.type === 'ai-title') {
      const t = o.title || o.aiTitle || o.content;
      if (typeof t === 'string' && t.trim()) title = t.trim();
    }
    if (!slug && typeof o.slug === 'string') slug = o.slug;
    if (!firstPrompt && o.type === 'user' && !o.isMeta && o.message &&
        typeof o.message.content === 'string') {
      const cleaned = cleanPrompt(o.message.content);
      if (cleaned) firstPrompt = cleaned;
    }
  }
  info.title = title;
  info.label = title || firstPrompt || slug || sessionId.slice(0, 8);
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
    const label = truncate(n.label, 40).padEnd(labelW);
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
  const w = (s) => out.write(s);

  let restored = false;
  function restore() {
    if (restored) return;
    restored = true;
    try { if (inp.setRawMode) inp.setRawMode(false); } catch { /* ignore */ }
    w('\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l'); // mouse off
    w('\x1b[?25h');   // cursor on
    w('\x1b[?1049l'); // leave alt screen
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
    const marker = n.current ? '  ← current' : n.latest ? '  (most recent)' : '';
    const fixed = `${idx}  ${r.prefix}${r.connector}● ${sid}  `;
    const room = Math.max(8, cols - fixed.length - marker.length);
    const label = truncate(n.label, room);
    let text = `${fixed}${label}${marker}`;
    if (text.length > cols) text = text.slice(0, cols);
    if (isSel) return '\x1b[7m' + text.padEnd(cols) + '\x1b[0m';
    // non-selected: colorize the glyph + id
    const glyph = n.current ? green('●') : n.latest ? yellow('●') : '●';
    return `${idx}  ${r.prefix}${r.connector}${glyph} ${cyan(sid)}  ${label}` +
      (marker ? dim(marker) : '');
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
    if (node.title) lines.push(bold(truncate(node.title, width)));
    const body = node.promptFull || node.label || dim('(no prompt text)');
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

  function onData(data) {
    let i = 0;
    while (i < data.length) {
      const rest = data.slice(i);
      const m = /^\x1b\[<(\d+);(\d+);(\d+)([Mm])/.exec(rest);
      if (m) { handleMouse(+m[1], +m[2], +m[3], m[4]); i += m[0].length; continue; }
      if (rest.startsWith('\x1b[A')) { move(-1); i += 3; continue; }
      if (rest.startsWith('\x1b[B')) { move(1); i += 3; continue; }
      if (rest.startsWith('\x1b[H')) { selected = 0; render(); i += 3; continue; }
      if (rest.startsWith('\x1b[F')) { selected = rows.length - 1; render(); i += 3; continue; }
      const ch = data[i];
      if (ch === '\r' || ch === '\n') { resume(rows[selected].node); return; }
      if (ch === 'k') { move(-1); i++; continue; }
      if (ch === 'j') { move(1); i++; continue; }
      if (ch === 'g') { selected = 0; render(); i++; continue; }
      if (ch === 'G') { selected = rows.length - 1; render(); i++; continue; }
      if (ch === 'q' || ch === '\x03' || ch === '\x1b') { quit(0); return; }
      i++;
    }
  }

  inp.setRawMode(true);
  inp.resume();
  inp.setEncoding('utf8');
  w('\x1b[?1049h\x1b[?25l');          // alt screen + hide cursor
  w('\x1b[?1000h\x1b[?1003h\x1b[?1006h'); // mouse: buttons + any-motion + SGR
  render();
  inp.on('data', onData);
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
