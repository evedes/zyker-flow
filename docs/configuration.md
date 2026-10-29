# Configuration

## `.flow.json`

```json
{
  "work_dir": "flow",
  "protected_paths": ["lib/", "test/", "mix.exs"],
  "standing_gates": ["mix test"],
  "codegen": ["\\bmix\\s+(phx|ecto)\\.gen\\.\\S", "\\bmix\\s+format\\b(?!.*--check-formatted)"],
  "deploy": {
    "provider": "forgejo",
    "branch": "main",
    "forgejo_url": "https://forgejo.example.com",
    "repo": "owner/app",
    "token_env": "FORGEJO_TOKEN",
    "health_url": "https://app.example.com/health"
  }
}
```

| Key | Meaning |
|---|---|
| `work_dir` | where feature records live (default `flow`; `flow init` used to write `work`) |
| `protected_paths` | path prefixes that count as code, relative to the git root (`./lib/` is `lib`); an absolute one, one with `..` or a glob, and one naming the whole repo are errors |
| `standing_gates` | shell commands `flow gates` runs at the git root, before every ticket's verification commands |
| `codegen` | regexes ([fancy-regex](https://docs.rs/fancy-regex) syntax) for commands that write code without naming the files (generators, formatters); each is searched for in one simple command at a time |
| `deploy` | optional; without it the push check and the ship→done CI check are skipped (see [Deploy providers](#deploy-providers)) |
| `judge` | optional: `model` and `max_budget_usd` for `flow judge` (see [The judge](judge.md)) |

`flow init` fills in `protected_paths`, `standing_gates` and `codegen` for
Elixir (`mix.exs`), Rust (`Cargo.toml`), Python (`pyproject.toml`) and Node
(`package.json`), checked in that order; with none of them it guesses `src/`
and no gates. They are starting points: check them.

`codegen` patterns are compiled as written by fancy-regex: `\b`, `\s`, `\S`,
lookarounds, backreferences, named groups (`(?<name>…)`) and Unicode classes
(`\p{Greek}`) work, and syntax it lacks (the `(?a)` flag, say) is a compile
error. There is no backtracking limit, so a catastrophic pattern holds the hook
until Claude Code's hook timeout.

### An invalid config

The config is read on every hook call. If it is not valid JSON, a known key has
the wrong type, a `codegen` pattern doesn't compile
(`.flow.json codegen pattern '<p>': <error>`) or a protected path is one of
those errors (`.flow.json protected path '<p>': <why>`), every Edit/Write/Bash
call is denied with `hook error: …` and the CLI fails, `pause` included. Fix
`.flow.json` by hand (an editor or a `!` command), not through the agent.

## Deploy providers

| `provider` | Keys | Runs come from |
|---|---|---|
| `forgejo` (the default when `forgejo_url` is set) | `forgejo_url`, `repo`, `token_env` (default `FORGEJO_TOKEN`) | Forgejo's `/actions/tasks` API, one entry per job |
| `github` | `repo` (optional: `gh` infers it from the remote) | `gh run list`, one entry per workflow run; needs `gh auth login` |
| `command` | `status_command` | a shell command, run with `/bin/sh` at the git root and stdin closed, that prints a JSON list of `{"name", "status", "head_sha"}` (`"id"` optional) |

Every provider also takes `branch` (default `main`) and `health_url`
(optional; it must answer 200 before ship→done). Statuses `running`,
`waiting`, `blocked`, `queued`, `pending`, `requested` and `in_progress` are in
flight, `success` and `skipped` pass, anything else is a failure.

The push check looks at the last 30 runs on any branch: one in flight anywhere
refuses the push, and so does a provider error (an unset token, Forgejo
unreachable). Ship→done fetches `origin/<branch>` and needs HEAD to be on it,
with every CI run for HEAD passed: flow assumes HEAD is pushed straight to
`branch`, so a squash-merged pull request never reaches done. A reply of the
wrong shape (Forgejo's not a JSON object or its `workflow_runs` not a list,
`gh` output missing fields) is a provider error; `flow deploy` shows a
completed `gh` run without a conclusion as `None`.

`flow deploy` lists the last 15 CI runs (`*` marks HEAD) and the health check.

HTTP (Forgejo and `health_url`) uses the proxies from the environment, the
system's root certificates, 15 s for the whole request and a new connection
per request. `Authorization` is kept on a same-host redirect only; the health
check sends `User-Agent: flow-health/1`.

## Templates

`research.md`, `spec.md`, `ticket.md` and `review.md` are scaffolded from the
plugin's `templates/` (and `STANDING.md` by `flow init`). A repo overrides any
of them with a file of the same name in `<work_dir>/templates/`.

The template decides what `flow check` requires: every `##` section it seeds
with a `TODO(flow)` placeholder must be present and filled in. `{title}`,
`{id}`, `{work_dir}` and, in `review.md`, `{fingerprint}` (the last gate run's,
for the record; nothing checks it) are substituted when scaffolding. A `## `
line inside a code fence (``` or ~~~) is text, not a heading.

The CLI reads a few sections by name, so overrides should keep them:

- `## Open questions` (research and spec): unticked `- [ ]` items block
  advancing.
- `## Verification steps` (tickets): the commands in its ``` block, one per
  line, `#` lines skipped.
- `## Spec changes` (spec): see [The living spec](living-spec.md).
- `## Verdict` (review): must start with `pass`, unless review is delegated:
  then `review.md` is the judge's record and the template is not checked.

Phase artifacts are scaffolded on entry: `research.md` by `flow new`;
`spec.md`, `tickets/T01.md` (unless `tickets/` already holds tickets) and
`review.md` by `flow advance`. Rename `T01.md` to `T01-<slug>.md` and copy it
for further tickets. Each `flow advance` into review renames an existing
`review.md` to `review-<n>.md` and scaffolds a fresh one, so coming back to
review after a code change always needs a new verdict.

## Files in a flow repo

- `.flow.json` at the git root.
- `<work_dir>/STANDING.md`: the standing Definition of Ready/Done every ticket
  inherits.
- `<work_dir>/templates/` (optional): template overrides.
- `<work_dir>/specs/<area>.md`: the living spec (see
  [The living spec](living-spec.md)).
- `<work_dir>/flow.json` (active feature, paused flag) and
  `<work_dir>/<NNN-slug>/`: `state.json`, `research.md`, `spec.md`,
  `tickets/T01-*.md`, `review.md` (earlier rounds as `review-<n>.md`).
  Committed with the feature, as its record: ship→done refuses while that
  folder has uncommitted changes.

flow rewrites the two state files pretty-printed (non-ASCII as UTF-8), keeping
keys it doesn't know; a known key of the wrong type, or an unknown phase, fails
every command and hook. `flow advance` to done changes `state.json`, merges
the spec changes into `<work_dir>/specs/`, and commits both (see
[The living spec](living-spec.md)).

## Command details

- `flow init` refuses if `.flow.json` exists.
- `flow new <slug> ["<title>"]` starts feature `NNN-<slug>`; a leading `NNN-`
  in the slug is dropped, and the title defaults to the slug. It refuses while
  the repo has code and no living spec.
- `flow check` exits 1 while the next phase is blocked.
- `flow gates` runs only in build, and refuses while an approval is stale.
- `flow approve` works only in the artifact's own phase, and only when the
  artifact is complete.
- `flow pause` suspends enforcement; the state files stay protected.
- `flow rewind <phase>` puts the feature's folder and the protected code back to
  the checkpoint taken on entering that phase, keeping the discarded work as
  `refs/flow/<id>/attempt-<n>`; it refuses once protected code was committed
  since (see [Rewinding a feature](rewind.md)).
- `flow abandon` leaves the feature's folder in place with phase `abandoned`;
  it also clears an active feature whose folder is gone.
