# flow

A phase-gated feature workflow for Claude Code. Every feature moves through:

```
explore -> spec -> tickets -> build -> review -> ship -> done
```

The order is enforced by code, not by instructions: the `flow` CLI owns the
state, and a `PreToolUse` hook asks it before every edit and Bash command.

## What is enforced

| Rule | Where |
|---|---|
| Code under `protected_paths` is edited only in **build** (Edit/Write tools, Bash writes, code generators like `mix phx.gen.*`) | hook |
| Code is committed only in **ship**, with both approvals valid, gates passed and review passed **on the exact code being committed** (content fingerprint) | hook on `git commit` |
| No `git push` while a CI/deploy run is in flight | hook on `git push` (Forgejo API) |
| Spec and tickets need the user's approval; editing them afterwards voids it (content hash) | CLI |
| A phase can't be left until its artifact is complete: required sections filled, no `TODO(flow)` placeholders, open questions ticked, ticket verification commands present | CLI (`flow check`) |
| The agent can't run `approve`, `pause`, `resume`, `abandon`, or edit the state files | hook + permission deny rules |

Changes that touch no protected path (docs, CI, tooling) are never blocked.

What hooks can't enforce is judgment: a thin spec can pass the section checks.
That's what the two user approvals and the fresh-context review are for. Bash
writes are detected heuristically, so the commit gate, which fingerprints the
content itself, is the real guarantee.

## Commands

| Command | Who | What |
|---|---|---|
| `flow new <slug> "<title>"` | agent | start a feature (one active at a time) |
| `flow status`, `flow check` | anyone | where things are, what blocks the next phase |
| `flow advance`, `flow back` | agent | move forward (when `check` passes) or back one phase |
| `flow gates` | agent | run standing gates + every ticket's verification commands |
| `flow deploy` | anyone | CI jobs for HEAD and the health check |
| `flow approve spec\|tickets` | user | approve the current artifact |
| `flow pause`, `flow resume` | user | suspend or restore enforcement (state files stay protected) |
| `flow abandon` | user | drop the active feature |

The user runs their commands with the `!` prefix in Claude Code (e.g.
`! plugins/flow/bin/flow approve spec`) or from a terminal.

## Files

Per repo:

- `.flow.json` at the git root opts the repo in (hooks are no-ops elsewhere):
  ```json
  {
    "work_dir": "work",
    "protected_paths": ["lib/", "test/", "mix.exs"],
    "standing_gates": ["mix test"],
    "deploy": {
      "branch": "main",
      "forgejo_url": "http://odin:3000",
      "repo": "edo/zyker",
      "token_env": "FORGEJO_CLAUDE_PAT",
      "health_url": "https://zyker.dev/health"
    }
  }
  ```
  `deploy` is optional; without it the push check and the ship→done deploy
  check are skipped.
- `work/STANDING.md`: the standing Definition of Ready/Done every ticket
  inherits (layer 1 of the agent ticket template).
- `work/flow.json` (active feature, paused flag) and `work/<NNN-slug>/`:
  `state.json`, `research.md`, `spec.md`, `tickets/T01-*.md`, `review.md`.
  Committed with the feature, as its record.

In the plugin: `bin/flow` (Python 3, stdlib only), `hooks/hooks.json`,
`skills/flow/SKILL.md` (how the agent runs each phase), `templates/`.

## Installing

Inside zyker it's wired through `.claude/settings.json` (hooks and deny rules)
and `.claude/skills/flow` (a symlink to the skill), because loading a plugin
from a directory inside the repo isn't clearly documented yet. Hooks load at
session start, so restart Claude Code after changing them.

To reuse it elsewhere, either copy that wiring, or move `plugins/flow` into its
own repo with a `.claude-plugin/marketplace.json` and install it as a plugin
(`hooks/hooks.json` and `bin/` on PATH then come with it). Don't do both in
one repo, or every hook runs twice.

## Tests

```sh
python3 -m unittest discover -s plugins/flow/tests
```
