# branch-graph

Visualize the **fork tree of your Claude Code sessions** and jump to any branch — with
**zero LLM tokens**. It's pure static local code; no model round-trip is involved.

When you run `/branch` (or `claude --fork-session`), Claude Code creates a *new session*
that copies the history up to a divergence point. Over time a project grows a tree of
forked sessions, but the built-in `/resume` picker is a flat list that doesn't show which
session branched from which. `branch-graph` reconstructs and draws that tree.

```
Branches in my-project:
  1  ●   5c2f8a74  Discuss validation options…       /resume 5c2f8a74-…
  2  ├─● 08fd2b0b  Try approach A…                   /resume 08fd2b0b-…
  3  └─● 99d8c3b9  Try approach B…                   /resume 99d8c3b9-…
  4     └─● 71642bc9  Refine B…            ← current (this session)
```

## Install

Requires Node.js (>= 16) on your PATH.

```sh
./install.sh                       # symlinks `branch-graph` into ~/.local/bin
BINDIR=/usr/local/bin ./install.sh # or choose another bin dir
```

If the install script warns that the bin dir isn't on your PATH, add it (e.g. to `~/.zshrc`):

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Uninstall:

```sh
./uninstall.sh
```

## Interactive picker (separate terminal)

The main way to use `branch-graph`. Run it in a normal terminal (not inside Claude Code)
and it opens an interactive picker over your project's fork tree:

```sh
branch-graph
```

This is what it looks like in a real terminal (captured from an actual run) — the
branch list on top, a detail panel for the selected branch below it, updating live as
you move the selection:

```
Branches in branch-graph-test

1  ● 615d27a5  branch-graph
2  ● 13de9b43  Test Claude Code branching feature
3  ├─● ac7d3bc7  b00 p0
4  │  ├─● a3c7d9c6  b000 p0
5  │  └─● 34e51181  This session is being continued from a previ…  (most recent)
6  ├─● c4edbf82  b01 p0
7  │  └─● 14a5a33c  b010 p0
8  └─● 706ac5db  B02

────────────────────────────────────────────────────────────────────────────────
34e51181-2b47-4a77-b88e-10b8ee8119b5
forked from ac7d3bc7 @ 8f12d678 · last active 6/29/2026, 9:30:32 PM

This session is being continued from a previous conversation that ran out of
context. The summary below covers the earlier portion of the conversation.
Summary: 1. Primary Request and Intent: The user is testing Claude Code's
branching feature. The explicit request was: "im testing branching feature of
calude code. just update the @file.txt with exactly what i typed from now
onwards. no explaintions. just do it" The user wants to verify that file state
…

↑/↓ or hover: navigate   Enter/click: resume   Esc: quit
```

Row 5 is the current selection — in the real terminal it's shown in reverse video
(inverted colors), which a plain-text block can't reproduce. The tree on the left
mirrors the fork structure (`├─`/`└─`/`│`), bold labels are explicit titles/names
while plain ones are first-prompt text, and the detail panel below always reflects
whichever row is currently selected.

- **↑/↓** or **j/k**, or **mouse hover** — move the selection.
- The detail panel shows the focused branch's session id, fork point, last-active time,
  and more of its starting prompt.
- **Enter** or **left-click** a branch — **launches a Claude session on it**
  (`claude -r <sessionId>`), handing the terminal over. This is the key advantage of the
  separate terminal: the picker switches you to the branch directly, no copy-paste.
- **Esc** — quit.

It auto-activates whenever it's attached to a real terminal. Force it with
`-i` / `--interactive`, or disable it with `--no-interactive`. Piped output, `--json`,
`--list`, a numeric index, or running inside Claude Code all fall back to plain output.

> While the picker is open, mouse tracking captures clicks, so normal terminal
> text-selection is intercepted — hold **Option** (iTerm2) or **Shift** (most terminals)
> to select text instead.

### Flags

| Flag | Description |
| --- | --- |
| `-i`, `--interactive` | Force the interactive picker (requires a terminal). |
| `--no-interactive` | Force plain list output. |
| `--project <path>` | Inspect a different project: pass a working dir, or a `~/.claude/projects/<dir>` path. |
| `--json` | Machine-readable output (index, sessionId, parent, label, current, resume). |
| `--list` | Force the list view. |
| `--color` / `--no-color` | Force or disable ANSI color. Also honors `FORCE_COLOR` / `NO_COLOR`. |
| `-h`, `--help` | Show help. |

## Running inside a Claude Code session (side feature)

You can also run `branch-graph` from *within* a Claude Code session via the `!` prefix,
which runs it locally. The process itself uses zero LLM tokens, but its output lands
directly in the conversation, where Claude reads and interprets it, which does cost tokens:

```
!branch-graph        # list the fork tree, with a /resume line per branch
!branch-graph 2      # print only the /resume line for branch #2
```

It prints a ready `/resume <id>` for each branch; paste that to switch to the branch in
place, with no separate `claude` process.

**Limitations of this mode:**

- **No interactive picker, no auto-switch.** A command launched from the `!` prefix is a
  child process and cannot drive the Claude Code TUI, so it only *surfaces* the
  `/resume <id>` line — you do the switch by pasting that built-in slash command yourself.
  For navigation and one-key switching, use the picker in a separate terminal.
- Output is a static list: no detail panel, hover, or keyboard navigation.

Color still works here: it's detected via the `CLAUDECODE` environment variable (the `!`
prefix pipes stdout rather than using a TTY), so output stays colored. Plain pipes and file
redirects stay color-free. Override with `--color` / `--no-color` or the `FORCE_COLOR` /
`NO_COLOR` env vars.

## How it works

- Reads the session transcripts for the current project under
  `~/.claude/projects/<dir>/<sessionId>.jsonl`.
- Builds the tree from each forked session's `forkedFrom: { sessionId, messageUuid }`.
- Marks the current session via the `CLAUDE_CODE_SESSION_ID` environment variable
  (`← current (this session)`). If that session isn't part of the project being shown
  (e.g. you used `--project`, or ran it from a plain terminal), it instead flags the most
  recently written session as `(most recent)` so there's always a useful anchor.
- Surfaces a ready `/resume <id>` for each branch.

## Limitations

- Resumes whole **sessions** (`/branch` forks). There is no per-message resume in Claude Code.
- Within-session **rewind** branches are intentionally excluded — Claude Code has no way
  to navigate to abandoned rewind branches (no such feature exists yet; checked 2026-06-27).
- In the in-session (`!` prefix) mode, the tool surfaces the `/resume <id>` line but cannot
  perform the switch itself; the separate-terminal picker can.
