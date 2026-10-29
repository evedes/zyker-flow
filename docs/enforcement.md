# Enforcement

What flow enforces, and how its hooks do it.

## The rules

| Rule | Where |
|---|---|
| Code under `protected_paths` is edited only in **build** (Edit/Write tools, Bash writes, `codegen` commands) | hook |
| Code is committed only in **ship**, with both approvals valid, gates passed and review passed **on the exact code being committed** (content fingerprint) | hook on `git commit` |
| Every move into review starts a fresh `review.md`, and gates run only in build, so a verdict only covers the code it was written for | CLI |
| The gates' pass covers the spec and tickets they ran on as well as the code: a ticket added or changed since (its verification steps never ran) makes them stale | CLI |
| No `git push` while a CI/deploy run is in flight | hook on `git push` (the `deploy` provider) |
| Spec and tickets need the user's approval, or the judge's where the user delegated it; editing them afterwards voids it (content hash), and `flow gates` and every later phase refuse until they are restored or approved again. Each approval also records what it rests on: the tickets' approval the spec it was given on, the review's the spec and tickets, so changing the spec voids the tickets' approval too | CLI |
| A delegated review needs the judge's approval (or the user's) of the exact code; a verdict the agent writes in `review.md` doesn't count | CLI |
| While a feature is active, the rules it is judged by (`.flow.json`, `.claude/settings.json` and `.claude/settings.local.json`, which hold the hooks and deny rules, `<work_dir>/STANDING.md`, `<work_dir>/templates/`, `<work_dir>/specs/`) change only by hand | hook |
| The living spec (`<work_dir>/specs/`) changes only at done, by the spec changes the user approved, and only when they still apply; done commits it, and review checks the code against those changes | CLI |
| A phase can't be left until its artifact is complete: the sections its template requires are filled, no `TODO(flow)` placeholders, open questions ticked, ticket verification commands present | CLI (`flow check`) |
| The agent can't run `approve`, `delegate`, `pause`, `resume`, `rewind`, `abandon`, or edit the state files | hook + permission deny rules |

Changes that touch no protected path (docs, CI, tooling) are never blocked
from being made. They do count towards the gates, though: the fingerprint
covers every file git sees outside `work_dir` (tracked, or untracked and not
ignored), so a README edit or a stray scratch file after `flow gates` makes the
gates stale, and the commit is refused until they are rerun.

### Committing docs mid-feature

The commit hook judges a lone `git commit` by what is staged, so a docs commit
mid-feature goes through while code changes sit unstaged. Anything that can
commit more than the index is judged by the whole working tree instead:

- `-a`, `-p`, `-i`, `-o`, their long names, or any prefix of them git would
  take, such as `--pathspec-from`
- a pathspec
- an option before `commit`, such as `git -P commit`
- another command in the same call, such as `git add -A && git commit`
- a `$(…)`, backtick or `<(…)` substitution, which could run one

That includes the usual `-m "$(cat <<'EOF' …)"` message: for a docs commit
mid-feature, use a literal `-m`.

### What can't be enforced

What hooks can't enforce is judgment: a thin spec can pass the section checks.
That's what the two user approvals and the fresh-context review are for. Bash
writes are detected heuristically, so the commit gate, which fingerprints the
content itself, is the real guarantee.

flow keeps a cooperative agent in phase; it is not a sandbox against a hostile
one. Anything the agent can run can bypass a pattern match (a `python3 -c`
that rewrites `state.json`, say), and approvals are content hashes, not
signatures. See [Known limitations](limitations.md).

## How it works

```
Claude Code ──PreToolUse (Edit|Write|MultiEdit|NotebookEdit|Bash|Monitor)──> bin/flow hook pre-tool-use ──> allow / deny + reason
            ──SessionStart──────────────────────────────────────────> bin/flow hook session-start ──> phase + blockers + primer as context
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
  paths outside build. Bash and Monitor (which runs a shell command too): the
  command is split into simple commands (quotes, `&&`/`||`/`;`/`|`, heredocs
  fed to `cat` dropped as data unless `cat` pipes them on), then each is
  checked for:
  - user-only `flow` subcommands
  - redirects into code or state files
  - mutating tools (`rm`, `mv`, `cp`, `sed -i`, `git checkout`…, but not an
    option such as `docker run --rm`) that name a protected path: as written,
    after `./` or `$PWD/`, as an option's value such as `--target=lib`, or as
    an absolute or `~/` path that resolves into it (a `VAR=value` such as
    `MIX_ENV=test` names no path)
  - `codegen` patterns
  - `git commit` and `git push`
- **Session-start hook.** In a flow repo, it gives the agent one status line
  (the active feature, its phase and what blocks the next one, or that none is
  active) and a primer, whether or not the skill gets loaded: what flow is and
  what it holds the work to (approved intent, the exact code that passed gates
  and review, the rule files, the user's commands), the repo's protected paths,
  what to do in the current phase (or, with no feature active in a repo with
  code and no living spec, that it must be written first), the gates
  delegated to the judge, and to keep the user told what flow is doing: the
  phase, each advance or back, what blocks, which approval is needed, every
  denial and why.
- **Fail closed.** If the pre-tool-use hook fails (an error, a panic, a
  payload of the wrong shape, no binary to run), it denies the call with
  `hook error: …` instead of allowing it. Claude Code always sends a JSON
  object; stdin flow can't read (not UTF-8, or not JSON as serde_json reads
  it: no `NaN`, no lone surrogate escapes, no numbers beyond f64, at most 128
  levels deep) is denied in a repo with a `.flow.json`, judged from the hook's
  own working directory, and allowed elsewhere. The session-start hook stays
  silent on errors.
- **Root.** Each path and command is judged by the flow repo it acts in, so a
  session in one repo working on another gets the other's rules and state. An
  absolute path (an edited file, a redirect target, a word a mutating command
  names) belongs to the repo it resolves into; a relative one to the repo its
  command runs in: the session's working directory, moved by a `cd`/`pushd`
  earlier in the same call (one that can be followed: not to a directory that
  isn't there, nor one only the shell can work out) and by a git command's
  `-C`. That repo is the innermost git root with a `.flow.json` around the
  path, past nested repos without one (a dependency checked out in the tree).

Messages from the CLI and hooks say `flow` when the `flow` on PATH is this
plugin's `bin/flow` (or a short script that names it, like the
[wrapper](installing.md#flow-for-the-user)), else its path (see
[The flow binary](installing.md#the-flow-binary)).
