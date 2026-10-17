# flow

A phase-gated feature workflow for Claude Code. Every feature moves through:

```
explore -> spec -> tickets -> build -> review -> ship -> done
```

The order is enforced by code, not by instructions: the `flow` CLI owns the
state, and a `PreToolUse` hook asks it before every edit and Bash command.
Repos opt in with a `.flow.json` at the git root; everywhere else the hooks do
nothing.

## What is enforced

| Rule | Where |
|---|---|
| Code under `protected_paths` is edited only in **build** (Edit/Write tools, Bash writes, `codegen` commands) | hook |
| Code is committed only in **ship**, with both approvals valid, gates passed and review passed **on the exact code being committed** (content fingerprint) | hook on `git commit` |
| No `git push` while a CI/deploy run is in flight | hook on `git push` (the `deploy` provider) |
| Spec and tickets need the user's approval; editing them afterwards voids it (content hash) | CLI |
| A phase can't be left until its artifact is complete: the sections its template requires are filled, no `TODO(flow)` placeholders, open questions ticked, ticket verification commands present | CLI (`flow check`) |
| The agent can't run `approve`, `pause`, `resume`, `abandon`, or edit the state files | hook + permission deny rules |

Changes that touch no protected path (docs, CI, tooling) are never blocked.

What hooks can't enforce is judgment: a thin spec can pass the section checks.
That's what the two user approvals and the fresh-context review are for. Bash
writes are detected heuristically, so the commit gate, which fingerprints the
content itself, is the real guarantee.

## Commands

| Command | Who | What |
|---|---|---|
| `flow init` | anyone | opt the repo in: `.flow.json`, `<work_dir>/STANDING.md`, deny rules |
| `flow new <slug> "<title>"` | agent | start a feature (one active at a time) |
| `flow status`, `flow check` | anyone | where things are, what blocks the next phase |
| `flow advance`, `flow back` | agent | move forward (when `check` passes) or back one phase |
| `flow gates` | agent | run standing gates + every ticket's verification commands |
| `flow deploy` | anyone | CI runs for HEAD and the health check |
| `flow approve spec\|tickets` | user | approve the current artifact |
| `flow pause`, `flow resume` | user | suspend or restore enforcement (state files stay protected) |
| `flow abandon` | user | drop the active feature |

The user runs their commands with the `!` prefix in Claude Code (e.g.
`! flow approve spec`) or from a terminal. `!` commands don't go through the
hooks. A Claude slash command can't do this: it runs through the agent's
tools, which the hook blocks by design.

Messages from the CLI and hooks say `flow` when one is on PATH, else the full
path.

## `.flow.json`

```json
{
  "work_dir": "work",
  "protected_paths": ["lib/", "test/", "mix.exs"],
  "standing_gates": ["mix test"],
  "codegen": ["\\bmix\\s+(phx|ecto)\\.gen\\.\\S", "\\bmix\\s+format\\b(?!.*--check-formatted)"],
  "deploy": {
    "provider": "forgejo",
    "branch": "main",
    "forgejo_url": "http://odin:3000",
    "repo": "edo/zyker",
    "token_env": "FORGEJO_CLAUDE_PAT",
    "health_url": "https://zyker.dev/health"
  }
}
```

| Key | Meaning |
|---|---|
| `work_dir` | where feature records live (default `work`) |
| `protected_paths` | path prefixes that count as code |
| `standing_gates` | shell commands `flow gates` runs, before every ticket's verification commands |
| `codegen` | regexes for commands that write code without naming the files (generators, formatters); each is matched against one simple command at a time |
| `deploy` | optional; without it the push check and the ship→done CI check are skipped |

`flow init` fills in `protected_paths`, `standing_gates` and `codegen` for
Elixir (`mix.exs`), Rust (`Cargo.toml`), Python (`pyproject.toml`) and Node
(`package.json`). They are starting points: check them.

### Deploy providers

| `provider` | Keys | Runs come from |
|---|---|---|
| `forgejo` (the default when `forgejo_url` is set) | `forgejo_url`, `repo`, `token_env` (default `FORGEJO_TOKEN`) | Forgejo's `/actions/tasks` API, one entry per job |
| `github` | `repo` (optional: `gh` infers it from the remote) | `gh run list`, one entry per workflow run; needs `gh auth login` |
| `command` | `status_command` | a shell command, run at the git root, that prints a JSON list of `{"name", "status", "head_sha"}` (`"id"` optional) |

Every provider also takes `branch` (default `main`) and `health_url`
(optional; it must answer 200 before ship→done). Statuses `running`,
`waiting`, `blocked`, `queued`, `pending`, `requested` and `in_progress` are in
flight, `success` and `skipped` pass, anything else is a failure.

## Templates

`research.md`, `spec.md`, `ticket.md` and `review.md` are scaffolded from
`templates/` (and `STANDING.md` by `flow init`). A repo overrides any of them
with a file of the same name in `<work_dir>/templates/`.

The template decides what `flow check` requires: every `##` section it seeds
with a `TODO(flow)` placeholder must be present and filled in. `{title}`,
`{id}`, `{work_dir}` and, in `review.md`, `{fingerprint}` are substituted when
scaffolding. The CLI reads a few sections by name, so overrides should keep
them: `## Open questions` (research and spec; unticked `- [ ]` items block
advancing), `## Verification steps` (tickets; the commands in its ``` block)
and `## Verdict` (review; must start with `pass`).

## Files

Per repo:

- `.flow.json` at the git root.
- `<work_dir>/STANDING.md`: the standing Definition of Ready/Done every ticket
  inherits.
- `<work_dir>/templates/` (optional): template overrides.
- `<work_dir>/flow.json` (active feature, paused flag) and
  `<work_dir>/<NNN-slug>/`: `state.json`, `research.md`, `spec.md`,
  `tickets/T01-*.md`, `review.md`. Committed with the feature, as its record.

In the plugin: `bin/flow` (Python 3, stdlib only), `hooks/hooks.json`,
`skills/flow/SKILL.md` (how the agent runs each phase), `templates/`.

## Installing

This repo is both the plugin and a one-plugin marketplace (`agile-flywheel`).
Install it once and it serves every repo; the hooks do nothing in repos
without a `.flow.json`:

```sh
claude plugin marketplace add ~/Work/agile-flywheel
claude plugin install flow@agile-flywheel
```

A marketplace added from a local directory loads the plugin in place: commits
here (and uncommitted edits) reach every session started afterwards, or after
`/reload-plugins`. Added from a git URL instead (`claude plugin marketplace add
<url>.git`, keeping the `.git`), it is copied into the plugin cache at a commit;
pick up new commits with `claude plugin marketplace update agile-flywheel` and
`claude plugin update flow@agile-flywheel`. To try a working copy for one
session without installing: `claude --plugin-dir ~/Work/agile-flywheel`.

The plugin brings the hooks, the skill, and `bin/` on the Bash tool's PATH.
Two things stay per machine or per repo:

- **`flow` for the user.** `!` commands and terminals aren't documented to get
  the plugin's PATH, so put a wrapper on yours:
  ```sh
  #!/bin/sh
  # ~/.local/bin/flow
  exec "$HOME/Work/agile-flywheel/bin/flow" "$@"
  ```
- **Permission deny rules.** A plugin's settings can't set permissions, so
  `flow init` writes them into the repo's `.claude/settings.json`. The hook
  enforces the same things without them; they are a second layer.

Don't also wire the hooks in a repo's own `.claude/settings.json`: plugin and
project hooks both run, so every check would run twice.

To opt a repo in, run `flow init` at its root, then edit `.flow.json` and
`work/STANDING.md`.

## Tests

```sh
python3 -m unittest discover -s tests
```
