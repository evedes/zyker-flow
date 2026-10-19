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
  name a protected path (as written, after `./` or `$PWD/`, or as an absolute
  or `~/` path that resolves into it), `codegen` patterns, `git commit` and
  `git push`.
- **Fail closed.** If the pre-tool-use hook fails (an error, a panic, a
  payload of the wrong shape, no binary to run), it denies the call with
  `hook error: …` instead of allowing it. Claude Code always sends a JSON
  object; stdin flow can't read (not UTF-8, or not JSON as serde_json reads
  it: no `NaN`, no lone surrogate escapes, no numbers beyond f64, at most 128
  levels deep) is denied in a repo with a `.flow.json`, judged from the hook's
  own working directory, and allowed elsewhere. The session-start hook stays
  silent on errors.
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

Messages from the CLI and hooks say `flow` when the `flow` on PATH is this
plugin's `bin/flow` (or a short script that names it, like the wrapper below),
else its path (see [The flow binary](#the-flow-binary)).

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
| `codegen` | regexes ([fancy-regex](https://docs.rs/fancy-regex) syntax) for commands that write code without naming the files (generators, formatters); each is searched for in one simple command at a time |
| `deploy` | optional; without it the push check and the ship→done CI check are skipped |

`flow init` fills in `protected_paths`, `standing_gates` and `codegen` for
Elixir (`mix.exs`), Rust (`Cargo.toml`), Python (`pyproject.toml`) and Node
(`package.json`), checked in that order; with none of them it guesses `src/`
and no gates. They are starting points: check them.

`codegen` patterns are compiled as written by fancy-regex: `\b`, `\s`, `\S`,
lookarounds, backreferences, named groups (`(?<name>…)`) and Unicode classes
(`\p{Greek}`) work, and syntax it lacks (the `(?a)` flag, say) is a compile
error. There is no backtracking limit, so a catastrophic pattern holds the hook
until Claude Code's hook timeout.

The config is read on every hook call. If it is not valid JSON, a known key has
the wrong type, or a `codegen` pattern doesn't compile
(`.flow.json codegen pattern '<p>': <error>`), every Edit/Write/Bash call is
denied with `hook error: …` and the CLI fails, `pause` included. Fix
`.flow.json` by hand (an editor or a `!` command), not through the agent.

### Deploy providers

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

HTTP (Forgejo and `health_url`) uses the proxies from the environment, the
system's root certificates, 15 s for the whole request and a new connection
per request. `Authorization` is kept on a same-host redirect only; the health
check sends `User-Agent: flow-health/1`.

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
  `tickets/T01-*.md`, `review.md` (earlier rounds as `review-<n>.md`).
  Committed with the feature, as its record: ship→done refuses while that
  folder has uncommitted changes.
  flow rewrites the two state files pretty-printed (non-ASCII as UTF-8),
  keeping keys it doesn't know; a known key of the wrong type, or an unknown
  phase, fails every command and hook.
  `flow advance` to done changes `state.json` after the ship commit, so commit
  the work dir once more afterwards.

In the plugin:

| Path | What |
|---|---|
| `.claude-plugin/plugin.json` | plugin manifest (`flow`; no `version`, see `AGENTS.md`) |
| `.claude-plugin/marketplace.json` | the one-plugin marketplace `zyker-flow` (`source: "."`) |
| `bin/flow` | the launcher (POSIX sh): finds or fetches the flow binary and runs it (see [The flow binary](#the-flow-binary)) |
| `src/`, `Cargo.toml`, `Cargo.lock` | the CLI and both hooks, in Rust (one binary, `flow`; `templates/` are compiled in): `main.rs` dispatches, `commands.rs` and `hooks.rs` are the entry points, `checks.rs` what each phase needs, `shell.rs` reads Bash commands, `markdown.rs` the artifacts, `config.rs` the JSON files, `repo.rs` the repo and its fingerprints, `deploy.rs` CI runs and the health check, `paths.rs` and `error.rs` the rest |
| `rustfmt.toml` | `cargo fmt` settings |
| `.github/workflows/` | `ci.yml` (fmt, clippy and every test suite on Linux and macOS; the release pin on main) and `release.yml` (the four release binaries) |
| `VERSION`, `SHA256SUMS` | the release bin/flow runs, and its pinned checksums (see [Releasing](#releasing)) |
| `hooks/hooks.json` | `PreToolUse` on `Edit\|Write\|MultiEdit\|NotebookEdit\|Bash`, and `SessionStart` |
| `skills/flow/SKILL.md` | how the agent runs each phase |
| `templates/` | artifact skeletons; they decide what `flow check` requires |
| `tests/` | end-to-end tests against throwaway git repos: `flow.rs` (the CLI and hooks), `launcher.rs` (bin/flow), `smoke.rs` (the smoke check, and its `real_repos` run) |
| `scripts/smoke` | runs `tests/smoke.rs` against the real repos that use flow (see [Tests](#tests)) |
| `scripts/source-hash`, `scripts/pin-release` | the sources' hash a release records, and pinning a published release |

## Installing

Requirements: git and a POSIX sh; `gh` for the `github` provider. No Python,
and on Linux or macOS (x86_64 or aarch64) no Rust: bin/flow downloads the
release binary on first use (with `curl` or `wget`, checked with `sha256sum` or
`shasum`). On other Unix systems, and for a checkout whose sources have no
release, it builds flow with cargo (Rust 1.85 or newer). Windows is not
supported. See [The flow binary](#the-flow-binary).

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
zyker-flow` and `claude plugin update flow@zyker-flow`. A commit with
a new `VERSION` fetches that release on its first hook call; older ones stay
in the cache (`~/.cache/flow/<VERSION>/`) until deleted. To try a working copy
for one session without installing: `claude --plugin-dir ~/Work/zyker-flow`.

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

### The flow binary

`hooks/hooks.json` runs `bin/flow`, a POSIX sh launcher, on every tool call.
It runs the first of:

1. `$FLOW_BIN`, when set (an absolute path).
2. `target/release/flow` in the plugin, a build of this checkout. When a file
   under `src/` or `templates/`, `Cargo.toml` or `Cargo.lock` is newer, it is
   rebuilt first (`cargo build --release --locked`, its output on stderr; if it
   fails, the previous build runs, with a warning); a source saved while that
   build runs is built next time. A hook call outside a flow repo runs the
   previous build without rebuilding, since it prints nothing there.
3. The release binary of `VERSION` for this platform, cached in
   `${FLOW_CACHE_DIR:-${XDG_CACHE_HOME:-~/.cache}/flow}/<VERSION>/<target>/flow`
   and downloaded from the `v<VERSION>` GitHub release when missing. It must
   match the `SHA256SUMS` committed next to `VERSION` (so the plugin's commit
   pins the binary, not the release page), it must run here, and it must have
   been built from this checkout's sources (the `source` line of
   `SHA256SUMS`, as `scripts/source-hash` computes it; a `SHA256SUMS` without
   that line counts as other sources). Once verified, a stamp in `target/`
   naming that binary lets later calls skip the checks until the sources,
   `VERSION` or `SHA256SUMS` change.
4. Otherwise flow is built from source with cargo, into `target/` (step 2
   from then on), without downloading the release first. A checkout with
   unreleased changes to `src/` therefore builds itself; without cargo (or if
   the build fails) it runs the release built from other sources, with a
   warning on stderr.

With none of these the hooks fail closed: in a repo with a `.flow.json`,
pre-tool-use denies every call (`no flow binary: <why>`) and session-start
says why; elsewhere they do nothing, also offline. A binary that cannot be
executed (another platform's, a `noexec` mount, a corrupt file) is caught too:
in hook mode the launcher runs the binary as a child and denies when the
shell reports 126 or 127, and drops such a cached file. The pre-tool-use hook
also has `"onFailure": "block"`, so a launcher that fails or times out blocks
the call instead of letting it through. A download is tried once (60 s at
most) and a failure is remembered for five minutes, so an offline machine is
not held up on every call. `FLOW_BIN`, `FLOW_CACHE_DIR`, `XDG_CACHE_HOME` and
`HOME` are ignored unless they are absolute, since a relative one would
resolve inside the guarded repo.

The launcher exports `FLOW_SELF`, its own path with symlinks resolved, to the
binary it runs. flow uses it where it names itself (`flow` when the `flow` on
PATH is this launcher or a short script naming it, else this path, relative to
the git root when inside it); a binary run directly names its own path.

### Releasing

A release is what bin/flow runs for everyone without a build of their own, so
`VERSION` and `SHA256SUMS` change together, after the release exists:

1. On a branch, set `VERSION` and the `version` in `Cargo.toml` to the new
   version, commit, and push the tag `v<VERSION>` for that commit (not main).
   `.github/workflows/release.yml` builds the four binaries and publishes them
   with `SHA256SUMS`, whose `source` line is the hash of the tagged sources.
   A published release is never changed; a fix needs a new version.
2. `scripts/pin-release` copies that `SHA256SUMS` into the repo, after
   checking it lists all four binaries and this checkout's sources. Commit it,
   and merge the branch into main.

On main, CI checks that `VERSION`, `Cargo.toml` and `SHA256SUMS` agree with
the published release, and warns when `src/`, `templates/` or `Cargo.*` have
changed since (users without cargo then run the older release). Until the
first release is pinned, bin/flow builds flow from source everywhere.

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

Of the binary and its launcher:

- Release binaries exist only for Linux and macOS on x86_64 and aarch64; other
  Unix systems (the BSDs, other architectures) need cargo. flow does not build
  on Windows: it uses Unix APIs and runs gates and `status_command` with
  `/bin/sh`.
- When no release matches the checkout (before the first release, unreleased
  changes to `src/`, a dev checkout without `target/`), the first hook call
  builds flow with cargo inside the hook, about 40 s; the call waits, and is
  blocked if the hook times out.
- Input that is not UTF-8 (arguments, paths, symlink targets, hook stdin, file
  contents) is an error: the CLI exits 1, and pre-tool-use denies in a flow
  repo.
- The launcher compares mtimes with `test -nt`, which macOS's `/bin/sh` (bash
  3.2) does to the second: a source, `VERSION` or `SHA256SUMS` changed within
  the same second as the last build or check is noticed only once something
  changes again.

## Tests

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo build --release --locked
cargo test                                   # tests/flow.rs, launcher.rs, smoke.rs, unit tests
FLOW_TEST_SH=$(command -v zsh) cargo test --test launcher   # the launcher under another shell
scripts/smoke
```

`FLOW_TEST_SH` must be an absolute path; CI runs the launcher suite again
under bash on Linux and zsh on macOS. `FLOW_TEST_BIN=<absolute path> cargo
test --test flow` runs `tests/flow.rs` against another binary, such as
`$PWD/bin/flow`.

The plugin is live in every repo as soon as a change is saved here, so
`scripts/smoke` checks the change against the real ones: it runs this
checkout's `bin/flow` in every `~/Work` repo with a `.flow.json` (or the repos
given as arguments; `--flow PATH` tries another copy), runs `flow status` and
the session-start hook, and feeds the pre-tool-use hook a few tool calls whose
verdict follows from that repo's config and state. It writes no files, so it
is safe mid-feature (in ship, `flow status` does `git fetch` the deploy
branch). A `hook error` or a wrong verdict fails it. It is `tests/smoke.rs`'s
ignored `real_repos` test, run by cargo in release mode (so the launcher finds
`target/release/flow` fresh); the script passes `--flow` and the repos on as
`FLOW_SMOKE_BIN` and `FLOW_SMOKE_REPOS` (absolute, whitespace-separated).
