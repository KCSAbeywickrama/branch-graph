# branch-graph

Visualize the **fork tree of your Claude Code sessions** and jump to any branch — with
**zero LLM tokens**. It's pure static local code; no model round-trip is involved.

When you run `/branch` (or `claude --fork-session`), Claude Code creates a *new session*
that copies the history up to a divergence point. Over time a project grows a tree of
forked sessions, but the built-in `/resume` picker is a flat list that doesn't show which
session branched from which. `branch-graph` reconstructs and draws that tree.

```
Branches in my-project

1  ● a4f3d8c2  Draft initial README outline
2  ● 9b6e1f47  Design payment retry logic
3  ├─● c8a2b91d  stripe-webhooks
4  │  ├─● 5e7f3a06  Add signature verification for Stripe webhooks
5  │  └─● 1d4c9b82  Handle partial refunds and capture expiration (most recent)
6  └─● 7f2e8c15  paypal-webhooks
...
```

## Install

Requires Node.js (>= 16) on your PATH.

```sh
./install.sh                       # symlinks `branch-graph` into ~/.local/bin
BINDIR=/usr/local/bin ./install.sh # or choose another bin dir
```

This also installs `cbg`, a short alias for `branch-graph` — use whichever you prefer,
they're interchangeable everywhere in this README.

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
branch-graph   # or the short alias: cbg
```

Here's the same layout and spacing as the real implementation, with an example branch
tree picked to show every kind of label `branch-graph` can display — the branch list on
top, a detail panel for the selected branch below it, updating live as you move the
selection:

```
Branches in my-project

1  ● a4f3d8c2  Draft initial README outline  10/07  9:14AM
2  ● 9b6e1f47  Design payment retry logic  12/07  4:03PM
3  ├─● c8a2b91d  stripe-webhooks  13/07 10:22AM
4  │  ├─● 5e7f3a06  Add signature verification for Stripe webhoo…  14/07  1:47PM
5  │  └─● 1d4c9b82  Handle partial refunds and capture expiration  15/07  3:42PM
6  ├─● 7f2e8c15  paypal-webhooks  15/07  5:08PM
7  │  └─● b3a6d904  Wire up PayPal IPN handler  16/07 11:30AM
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

↑/↓/hover: navigate   s: search   p/←: parent   →: child   Enter/click: resume   Esc: quit
```

Row 5 is the current selection — shown in reverse video in a real terminal (a
plain-text block can't reproduce that), with the detail panel below always describing
whichever row is selected. The tree lines (`├─`/`└─`/`│`) mirror the fork structure
exactly as `/branch` created it, and each label follows Claude Code's own precedence
for naming a session:

- **Explicit branch name** — set with `/branch <name>` or `/rename`, shown bold. Rows 3
  (`stripe-webhooks`) and 6 (`paypal-webhooks`). A name you set always wins over
  Claude's generated title, so a `/rename` shows up here immediately.
- **AI-generated title** — Claude's own summary of the session (`aiTitle`), shown
  bold. Rows 1, 2, and 8.
- **First prompt** — falls back to the branch's own first typed message when it has
  no title or name, shown as plain text. Rows 4, 5, and 7.

(If none of those exist, `branch-graph` falls back further to a stored slug, then the
raw session id.) Labels are truncated to whatever room the row has left, as row 4 shows.

Trailing each label is when that branch was **last active**, in short `DD/MM h:mmAM` form,
so you can see at a glance which branches are fresh and which are stale. On the one row
that carries a marker, the marker takes that slot instead: row 8's `(most recent)` — the
newest session in the project — or `← current` when you launched `branch-graph` from
inside a session of this project. The full timestamp for the selected row is in the detail
panel below.

- **↑/↓** or **j/k**, or **mouse hover** — move the selection.
- **p** or **←** — jump the selection to the branch this one was forked from (its
  parent). On a root branch it does nothing.
- **→** — descend to this branch's most recent child. On a branch with no forks off it,
  it does nothing.
- The detail panel shows the focused branch's session id, fork point, full last-active
  time, and more of its starting prompt.
- **s** or **/** — search (see below).
- **Enter** or **left-click** a branch — **launches a Claude session on it**
  (`claude -r <sessionId>`), handing the terminal over. This is the key advantage of the
  separate terminal: the picker switches you to the branch directly, no copy-paste.
- **Esc** — quit.

### Search

Press **s** (or **/**) and type. The tree filters as you type, and each match keeps
its parent branches — greyed out — so you can still see where it hangs off:

```
Branches in my-project
search: paypal   2 of 8

2  ● 9b6e1f47  Design payment retry logic  12/07  4:03PM      ← grey (context only)
6  └─● 7f2e8c15  paypal-webhooks  15/07  5:08PM               ← selected
7     └─● b3a6d904  Wire up PayPal IPN handler  16/07 11:30AM
```

Only rows 6 and 7 match — the count says **2 of 8**, not 3, because row 2 is kept
purely as context: it's their parent, so the matches still have a place in the tree.
Context rows are **greyed out** so the real hits stand out, and the connectors are
redrawn for the smaller tree — row 6 becomes a `└─` once its siblings are gone.

The selection starts on the **first actual match** (row 6), never on a grey context
row, and stays put as you keep typing for as long as that branch still matches.
Context rows are still fully selectable and resumable if you want one.

- **↑/↓** still move the selection while you type — every other printable key is text,
  so `j`, `k`, `p` and `q` type instead of navigating.
- **Backspace** deletes, **Ctrl-U** clears the query.
- **Enter** accepts the filter and drops you back into normal navigation over the
  matching rows. Resuming a branch then takes a second **Enter**, so the handoff to
  `claude` is always deliberate.
- **Esc** unwinds one layer per press: cancel typing → clear the filter → quit. A
  search is never lost to a stray keypress.

Rows keep their original numbers while filtered, so a row still matches
`branch-graph <n>`.

Matching runs entirely locally — no tokens, no network — over branch names, titles,
first prompts, and session ids (not full transcript bodies). It is case-insensitive,
and word order doesn't matter: `auth flow` finds *"flow for authentication"*. Against
names and titles you can also skip letters, so `lcns` finds *"Choose license for
distribution"*; first prompts match on text you actually typed.

It auto-activates whenever it's attached to a real terminal. Force it with
`-i` / `--interactive`, or disable it with `--no-interactive`. Piped output, `--json`,
`--list`, a numeric index, or running inside Claude Code all fall back to plain output.

> While the picker is open, mouse tracking captures clicks, so normal terminal
> text-selection is intercepted — hold **Option** (iTerm2) or **Shift** (most terminals)
> to select text instead.

## Going up one level: `..`

The most common move — back up one fork level from the branch you were just on — skips the
picker entirely:

```sh
cbg ..          # or: cbg -p, cbg --parent
```

It reads as "up one level" exactly like `cd ..`, and no shell mangles it. In a terminal it
launches `claude -r` on the parent straight away, handing the terminal over, same as
pressing Enter in the picker. Piped, or inside Claude Code, it prints the `/resume <id>`
line instead — a nested `claude` would be the wrong thing there.

The branch it goes up *from* is the session you're inside when that session belongs to the
project being shown, otherwise the most recently written one — the same anchor the
`← current` and `(most recent)` markers use. If that branch is a tree root, or its parent
transcript isn't in this project, `..` says so and exits non-zero rather than guessing.

### Flags

| Flag | Description |
| --- | --- |
| `..`, `-p`, `--parent` | Go up one fork level from the most recent branch, with no picker and no row number. Takes precedence over the picker. |
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
!branch-graph ..     # print the /resume line for the parent of the most recent branch
!cbg                 # short alias, same behavior
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
- Takes each branch's last-active time from its transcript file's modification time —
  shown short (`15/08  1:05PM`) after the label in the picker, in full in the detail panel.
- Surfaces a ready `/resume <id>` for each branch.

## Limitations

- Resumes whole **sessions** (`/branch` forks). There is no per-message resume in Claude Code.
- Within-session **rewind** branches are intentionally excluded — Claude Code has no way
  to navigate to abandoned rewind branches (no such feature exists yet; checked 2026-06-27).
- In the in-session (`!` prefix) mode, the tool surfaces the `/resume <id>` line but cannot
  perform the switch itself; the separate-terminal picker can.
