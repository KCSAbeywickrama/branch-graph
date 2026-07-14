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

Here's the same layout and spacing as the real implementation, with an example branch
tree picked to show every kind of label `branch-graph` can display — the branch list on
top, a detail panel for the selected branch below it, updating live as you move the
selection:

```
Branches in my-project

1  ● a4f3d8c2  Draft initial README outline
2  ● 9b6e1f47  Design payment retry logic
3  ├─● c8a2b91d  stripe-webhooks
4  │  ├─● 5e7f3a06  Add signature verification for Stripe webhooks
5  │  └─● 1d4c9b82  Handle partial refunds and capture expiration
6  ├─● 7f2e8c15  paypal-webhooks
7  │  └─● b3a6d904  Wire up PayPal IPN handler
8  └─● e91f4a73  Add structured logging to webhook handlers  (most recent)

────────────────────────────────────────────────────────────────────────────────
1d4c9b82-3f6e-4a71-9c58-7b2e8f4a6d91
forked from c8a2b91d @ 4f8a2c17 · last active 7/15/2026, 3:42:18 PM

Handle partial refunds and capture expiration edge cases. When a Stripe
webhook reports a partial capture followed later by a refund, our local
order state machine currently double-fires the fulfillment webhook
because it doesn't check whether the capture was already reconciled. We
need to track capture state per payment intent and make the refund
handler idempotent so replayed webhook events don't trigger duplicate
…

↑/↓ or hover: navigate   Enter/click: resume   Esc: quit
```

Row 5 is the current selection — shown in reverse video in a real terminal (a
plain-text block can't reproduce that), with the detail panel below always describing
whichever row is selected. The tree lines (`├─`/`└─`/`│`) mirror the fork structure
exactly as `/branch` created it, and each label follows Claude Code's own precedence
for naming a session:

- **AI-generated title** — Claude's own summary of the session (`aiTitle`), shown
  bold. Rows 1, 2, and 8.
- **Explicit branch name** — set with `/branch <name>`, shown bold. Rows 3
  (`stripe-webhooks`) and 6 (`paypal-webhooks`).
- **First prompt** — falls back to the branch's own first typed message when it has
  no title or name, shown as plain text. Rows 4, 5, and 7.

(If none of those exist, `branch-graph` falls back further to a stored slug, then the
raw session id.) Row 8 also carries `← current`, marking the session you launched
`branch-graph` from.

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
