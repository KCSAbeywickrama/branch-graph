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

## How it works

- Reads the session transcripts for the current project under
  `~/.claude/projects/<dir>/<sessionId>.jsonl`.
- Builds the tree from each forked session's `forkedFrom: { sessionId, messageUuid }`.
- Marks the current session via the `CLAUDE_CODE_SESSION_ID` environment variable
  (`← current (this session)`). If that session isn't part of the project being shown
  (e.g. you used `--project`, or ran it from a plain terminal), it instead flags the most
  recently written session as `(most recent)` so there's always a useful anchor.
- Prints a ready `/resume <id>` for each branch.

> **Note:** `branch-graph` does not switch sessions itself — a command launched from
> Claude Code's `!` prefix is a child process and cannot drive the Claude Code TUI. It
> hands you the `/resume <id>` line; you paste that (the built-in slash command) to switch
> **in place**, with no separate `claude` process. Rewind/checkpoint branches are not shown
> because Claude Code cannot navigate to abandoned rewind branches.

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

## Usage

Inside a Claude Code session (zero tokens — runs locally via the `!` prefix):

```
!branch-graph        # list the fork tree, with a /resume line per branch
!branch-graph 2      # print only the /resume line for branch #2
```

Then paste the printed `/resume <id>` to switch to that branch in place.

### Interactive picker (separate terminal)

Run `branch-graph` in a normal terminal (not inside Claude Code) and it opens an
interactive picker:

- **↑/↓** or **j/k**, or **mouse hover** — move the selection.
- The detail panel shows the focused branch's session id, fork point, last-active time,
  and more of its starting prompt.
- **Enter** or **left-click** a branch — launches a Claude session on it
  (`claude -r <sessionId>`), handing the terminal over.
- **Esc** — quit.

It auto-activates only when attached to a real terminal; piped, `--json`, `--list`, a
numeric index, or running inside Claude Code all fall back to plain output. Force it with
`-i` / `--interactive`, or disable it with `--no-interactive`.

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

### Color

Color is on automatically in a real terminal **and inside Claude Code** (detected via the
`CLAUDECODE` environment variable, since the `!` prefix pipes stdout rather than using a
TTY). Plain pipes and file redirects stay color-free. Override with `--color` / `--no-color`
or the `FORCE_COLOR` / `NO_COLOR` env vars.

## Limitations

- Switches happen via the built-in `/resume <id>` (which you send); the tool only surfaces it.
- Resumes whole **sessions** (`/branch` forks). There is no per-message resume in Claude Code.
- Within-session **rewind** branches are intentionally excluded — they aren't navigable.
