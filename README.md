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
| Every move into review starts a fresh `review.md`, and gates run only in build, so a verdict only covers the code it was written for | CLI |
| No `git push` while a CI/deploy run is in flight | hook on `git push` (the `deploy` provider) |
| Spec and tickets need the user's approval; editing them afterwards voids it (content hash), and `flow gates` and every later phase refuse until they are restored or approved again | CLI |
| A phase can't be left until its artifact is complete: the sections its template requires are filled, no `TODO(flow)` placeholders, open questions ticked, ticket verification commands present | CLI (`flow check`) |
| The agent can't run `approve`, `pause`, `resume`, `abandon`, or edit the state files | hook + permission deny rules |

Changes that touch no protected path (docs, CI, tooling) are never blocked
from being made. They do count towards the gates, though: the fingerprint
covers every file git sees outside `work_dir` (tracked, or untracked and not
ignored), so a README edit or a stray scratch file after `flow gates` makes the
gates stale, and the commit is refused until they are rerun.

The commit hook judges a lone `git commit` by what is staged, so a docs commit
mid-feature goes through while code changes sit unstaged. Anything that can
commit more than the index (`-a`, `-p`, `-i`, `-o`, a pathspec, another
command in the same call, such as `git add -A && git commit`, or a `$(…)`,
backtick or `<(…)` substitution, which could run one) is judged by the whole
working tree instead. That includes the usual `-m "$(cat <<'EOF' …)"` message:
for a docs commit mid-feature, use a literal `-m`.

What hooks can't enforce is judgment: a thin spec can pass the section checks.
That's what the two user approvals and the fresh-context review are for. Bash
writes are detected heuristically, so the commit gate, which fingerprints the
content itself, is the real guarantee.

flow keeps a cooperative agent in phase; it is not a sandbox against a hostile
one. Anything the agent can run can bypass a pattern match (a `python3 -c`
that rewrites `state.json`, say), and approvals are content hashes, not
signatures. See [Known limitations](#known-limitations).

## How it works

```
Claude Code ──PreToolUse (Edit|Write|MultiEdit|NotebookEdit|Bash)──> bin/flow hook pre-tool-use ──> allow / deny + reason
            ──SessionStart──────────────────────────────────────────> bin/flow hook session-start ──> phase + blockers as context
agent / user ──> bin/flow <command> ──> <work_dir>/flow.json, <work_dir>/<NNN-slug>/state.json + artifacts
```

- **State.** `<work_dir>/flow.json` holds the active feature and the paused
  flag; `<work_dir>/<NNN-slug>/state.json` holds the phase, the approvals
  (artifact hash + time), the last gate run (result + fingerprint), the
  reviewed fingerprint and a history of phase changes. Only the CLI writes
  them.
- **Fingerprint.** A SHA-256 over the path and content of every file
  `git ls-files -co --exclude-standard` lists, minus `work_dir`. `flow gates`
  records it; the review→ship move records it again as the reviewed
  fingerprint; `git commit` in ship needs the current tree to match both.
- **Pre-tool-use hook.** Edit/Write tools: deny state files always, protected
  paths outside build. Bash: the command is split into simple commands (quotes,
  `&&`/`||`/`;`/`|`, heredocs fed to `cat` dropped as data unless `cat` pipes
  them on), then each is
  checked for user-only `flow` subcommands, redirects into code or state
  files, mutating tools (`rm`, `mv`, `cp`, `sed -i`, `git checkout`…) that
  name a protected path, `codegen` patterns, `git commit` and `git push`.
- **Fail closed.** If the pre-tool-use hook raises, it denies the call with
  `hook error: …` instead of allowing it. The session-start hook stays silent
  on errors.
- **Root.** The hooks find the repo from the session's working directory
  (`git rev-parse --show-toplevel`), not from the file being edited.

## Commands

| Command | Who | What |
|---|---|---|
| `flow init` | anyone | opt the repo in: `.flow.json`, `<work_dir>/STANDING.md`, deny rules (refuses if `.flow.json` exists) |
| `flow new <slug> ["<title>"]` | agent | start a feature `NNN-<slug>` (one active at a time; a leading `NNN-` in the slug is dropped); the title defaults to the slug |
| `flow status`, `flow check` | anyone | where things are, what blocks the next phase (`check` exits 1 while blocked) |
| `flow advance`, `flow back` | agent | move forward (when `check` passes) or back one phase |
| `flow gates` | agent | in build: run standing gates + every ticket's verification commands, record result and fingerprint (refuses while an approval is stale) |
| `flow deploy` | anyone | the last 15 CI runs (`*` marks HEAD) and the health check |
| `flow approve spec\|tickets` | user | approve the current artifact (only in its own phase, and only when it is complete) |
| `flow pause`, `flow resume` | user | suspend or restore enforcement (state files stay protected) |
| `flow abandon` | user | drop the active feature (its folder stays, phase `abandoned`; also clears an active feature whose folder is gone) |

The user runs their commands with the `!` prefix in Claude Code (e.g.
`! flow approve spec`) or from a terminal. `!` commands don't go through the
hooks. A Claude slash command can't do this: it runs through the agent's
tools, which the hook blocks by design.

Messages from the CLI and hooks say `flow` when the `flow` on PATH is this one
(or a short script that names it, like the wrapper below), else the full path.

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
| `standing_gates` | shell commands `flow gates` runs at the git root, before every ticket's verification commands |
| `codegen` | regexes for commands that write code without naming the files (generators, formatters); each is matched against one simple command at a time |
| `deploy` | optional; without it the push check and the ship→done CI check are skipped |

`flow init` fills in `protected_paths`, `standing_gates` and `codegen` for
Elixir (`mix.exs`), Rust (`Cargo.toml`), Python (`pyproject.toml`) and Node
(`package.json`), checked in that order; with none of them it guesses `src/`
and no gates. They are starting points: check them.

The config is read on every hook call. If it is not valid JSON, or a `codegen`
pattern doesn't compile, every Edit/Write/Bash call is denied with
`hook error: …` and the CLI fails, `pause` included. Fix `.flow.json` by hand
(an editor or a `!` command), not through the agent.

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

The push check looks at the last 30 runs on any branch: one in flight anywhere
refuses the push, and so does a provider error (an unset token, Forgejo
unreachable). Ship→done fetches `origin/<branch>` and needs HEAD to be on it,
with every CI run for HEAD passed: flow assumes HEAD is pushed straight to
`branch`, so a squash-merged pull request never reaches done.

## Templates

`research.md`, `spec.md`, `ticket.md` and `review.md` are scaffolded from
`templates/` (and `STANDING.md` by `flow init`). A repo overrides any of them
with a file of the same name in `<work_dir>/templates/`.

The template decides what `flow check` requires: every `##` section it seeds
with a `TODO(flow)` placeholder must be present and filled in. `{title}`,
`{id}`, `{work_dir}` and, in `review.md`, `{fingerprint}` (the last gate run's,
for the record; nothing checks it) are substituted when scaffolding. The CLI
reads a few sections by name, so overrides should keep them: `## Open
questions` (research and spec; unticked `- [ ]` items block advancing),
`## Verification steps` (tickets; the commands in its ``` block, one per line,
`#` lines skipped) and `## Verdict` (review; must start with `pass`).

Phase artifacts are scaffolded on entry: `research.md` by `flow new`,
`spec.md`, `tickets/T01.md` (unless `tickets/` already holds tickets) and
`review.md` by `flow advance`. Rename `T01.md` to `T01-<slug>.md` and copy it
for further tickets. Each `flow advance` into review renames an existing
`review.md` to `review-<n>.md` and scaffolds a fresh one, so coming back to
review after a code change always needs a new verdict.

## Files

Per repo:

- `.flow.json` at the git root.
- `<work_dir>/STANDING.md`: the standing Definition of Ready/Done every ticket
  inherits.
- `<work_dir>/templates/` (optional): template overrides.
- `<work_dir>/flow.json` (active feature, paused flag) and
  `<work_dir>/<NNN-slug>/`: `state.json`, `research.md`, `spec.md`,
  `tickets/T01-*.md`, `review.md` (earlier rounds as `review-<n>.md`). Committed with the feature, as its record:
  ship→done refuses while that folder has uncommitted changes.
  `flow advance` to done changes `state.json` after the ship commit, so commit
  the work dir once more afterwards.

In the plugin:

| Path | What |
|---|---|
| `.claude-plugin/plugin.json` | plugin manifest (`flow`; no `version`, see `AGENTS.md`) |
| `.claude-plugin/marketplace.json` | the one-plugin marketplace `zyker-flow` (`source: "."`) |
| `bin/flow` | the CLI and both hooks: Python 3.8+, stdlib only, one file |
| `hooks/hooks.json` | `PreToolUse` on `Edit\|Write\|MultiEdit\|NotebookEdit\|Bash`, and `SessionStart` |
| `skills/flow/SKILL.md` | how the agent runs each phase |
| `templates/` | artifact skeletons; they decide what `flow check` requires |
| `tests/test_flow.py` | end-to-end tests against throwaway git repos |
| `scripts/smoke` | the hooks against the real repos that use flow |

## Installing

Requirements: Python 3.8+ and git on PATH; `gh` for the `github` provider.

This repo (`~/Work/zyker-flow`, GitHub `evedes/zyker-flow`) is both the plugin
and a one-plugin marketplace (`zyker-flow`). Install it once and it serves
every repo; the hooks do nothing in repos without a `.flow.json`:

```sh
claude plugin marketplace add ~/Work/zyker-flow
claude plugin install flow@zyker-flow
```

A marketplace added from a local directory loads the plugin in place
(`claude plugin list --json` shows `readFromFolder: ~/Work/zyker-flow`):
commits here, and uncommitted edits, reach every session started afterwards,
or after `/reload-plugins`. Claude Code still keeps a copy per commit under
`plugins/cache/zyker-flow/flow/<sha>/` for its records; the hooks don't
run from it. Added from a git URL instead (`claude plugin marketplace add
git@github.com:evedes/zyker-flow.git`), it is copied into the plugin cache at
a commit; pick up new commits with `claude plugin marketplace update
zyker-flow` and `claude plugin update flow@zyker-flow`. To try a
working copy for one session without installing:
`claude --plugin-dir ~/Work/zyker-flow`.

The plugin brings the hooks, the skill, and `bin/` on the Bash tool's PATH.
Two things stay per machine or per repo:

- **`flow` for the user.** `!` commands and terminals aren't documented to get
  the plugin's PATH, so put a wrapper on yours:
  ```sh
  #!/bin/sh
  # ~/.local/bin/flow
  exec "$HOME/Work/zyker-flow/bin/flow" "$@"
  ```
- **Permission deny rules.** A plugin's settings can't set permissions, so
  `flow init` writes them into the repo's `.claude/settings.json`:
  `Bash(flow approve:*)` and the other user-only commands, plus
  `Edit(<work_dir>/flow.json)` and `Edit(<work_dir>/*/state.json)` (an `Edit`
  rule covers Write and NotebookEdit too; Claude Code ignores `Write(path)`
  rules). The hook enforces the same things without them; they are a second
  layer.

Don't also wire the hooks in a repo's own `.claude/settings.json`: plugin and
project hooks both run, so every check would run twice.

To opt a repo in, run `flow init` at its root, then edit `.flow.json` and
`work/STANDING.md`. Start Claude Code at (or under) that root: the hooks only
apply when the session's working directory is inside the repo.

## Known limitations

Open defects, found in the 2026-10-18 review; each needs a fix and a test.

- `.flow.json`, `<work_dir>/templates/` and `STANDING.md` are not protected:
  the agent can edit them in any phase, changing the rules mid-feature.
- `flow gates` fingerprints the tree after the commands run, so a gate that
  rewrites code (an unchecked formatter, a generator) passes on code it never
  checked.
- Without a `deploy` section, ship→done only checks that the tree is clean,
  not that the reviewed code is what was committed.
- Only `git commit` (and `commit-tree`) is gated; `merge`, `cherry-pick`,
  `revert`, `am`, `rebase` and `pull` are not.
- Bash write detection misses `&>`/`&>>` redirects and `cd dir && …` into a
  protected path, resolves relative paths against the git root rather than the
  shell's cwd, and denies copies *out of* protected paths
  (`cp lib/x /tmp/`).
- The feature's state is untracked until it is first committed, and the hooks
  allow `git stash -u` and `git clean`, which remove it. flow then reports no
  active feature, or, when a tracked `<work_dir>/flow.json` survives, that the
  active feature's state is missing, and denies code edits until it is restored
  or the user runs `flow abandon`.
- A `## ` line inside a fenced block is read as a section heading.

## Tests

```sh
python3 -m unittest discover -s tests
scripts/smoke
```

The plugin is live in every repo as soon as a change is saved here, so
`scripts/smoke` checks the change against the real ones: it runs this
checkout's `bin/flow` in every `~/Work` repo with a `.flow.json` (or the repos
given as arguments; `--flow PATH` tries another copy), runs `flow status` and
the session-start hook, and feeds the pre-tool-use hook a few tool calls whose
verdict follows from that repo's config and state. It writes no files, so it
is safe mid-feature (in ship, `flow status` does `git fetch` the deploy
branch). A `hook error` or a wrong verdict fails it.
