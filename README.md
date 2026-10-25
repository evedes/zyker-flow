# flow

**A phase-gated feature workflow for Claude Code.** Every feature moves
through the same phases, in order:

```
explore -> spec -> tickets -> build -> review -> ship -> done
```

The order is enforced by code, not by instructions. The `flow` CLI owns the
state, and a hook checks every edit and shell command against it. So the agent
can't write code before you've approved what it's building, and can't commit
code that hasn't passed the gates and review.

Repos opt in with a `.flow.json` at the git root. Everywhere else the hooks do
nothing.

## What you get

- **Code changes only in build.** Files under your `protected_paths` can't be
  edited, overwritten from the shell or regenerated in any other phase.
- **Commits only in ship.** A `git commit` goes through only once you have
  approved the spec and tickets and both the gates and the review have passed,
  all on the exact code being committed (a content fingerprint).
- **Approvals stick to what you approved.** An approval is bound to the
  artifact's content, so editing an approved spec or ticket voids it.
- **The rules stay fixed.** While a feature is active, the agent can't edit
  `.flow.json`, the Claude settings, the standing agreement, the templates or
  the living spec.
- **You keep the keys.** Only you can approve, delegate, pause, resume or
  abandon. The agent can't run those commands or edit flow's state files.
- **A living spec.** `<work_dir>/specs/` describes how the system behaves now.
  Each feature declares what it changes in it, and flow merges those changes
  when the feature is done.
- **Fails closed.** If the hook breaks, it denies the call rather than letting
  it through.

Changes that touch no protected path (docs, CI, tooling) are never blocked.

> flow keeps a **cooperative** agent in phase. It is not a sandbox against a
> hostile one: anything the agent can run can get past a pattern match, and
> approvals are content hashes, not signatures. Hooks can't judge quality
> either, so a thin spec can still pass the checks. Your approvals and the
> fresh-context review cover that. See [Known limitations](docs/limitations.md).

## Quick start

You need git and a POSIX sh, on Linux or macOS. Rust is only needed on other
platforms (see [Installing](docs/installing.md)).

**1. Install the plugin** (once per machine):

```sh
git clone https://github.com/evedes/zyker-flow.git ~/zyker-flow
claude plugin marketplace add ~/zyker-flow
claude plugin install flow@zyker-flow
```

**2. Put `flow` on your PATH**, so you can run it from a terminal and with
Claude Code's `!` prefix:

```sh
#!/bin/sh
# ~/.local/bin/flow
exec "$HOME/zyker-flow/bin/flow" "$@"
```

**3. Opt a repo in**, at its root:

```sh
flow init
```

This writes `.flow.json` (with guessed settings for Elixir, Rust, Python or
Node), `flow/STANDING.md` (your standing Definition of Ready/Done) and the
permission deny rules. Check both files and commit them.

If the repo already has code, ask Claude to write the living spec before the
first feature. The skill knows the steps (see
[The living spec](docs/living-spec.md)).

**4. Work as usual.** Ask Claude for a feature. It runs `flow new` and works
through the phases, stopping when it needs you:

```
! flow approve spec
! flow approve tickets
```

## The phases

| Phase | What the agent does | What moves it on |
|---|---|---|
| **explore** | reads the code and living spec, then writes `research.md` | its open questions are answered |
| **spec** | writes `spec.md`: behaviour, acceptance criteria, changes to the living spec | you run `! flow approve spec` |
| **tickets** | splits the spec into `tickets/T01-….md`, each with verification commands | you run `! flow approve tickets` |
| **build** | writes the tests, then the code; the only phase where code can change | `flow gates` passes (standing gates + every ticket's checks) |
| **review** | a fresh subagent reviews the diff against the spec, in `review.md` | the verdict is `pass` |
| **ship** | commits, and pushes if a deploy is configured | CI is green and the health check passes (or, with no deploy, everything is committed) |
| **done** | flow merges the spec changes into the living spec and commits them | — |

A phase can't be left until its artifact is complete: every required section
is filled in and no `TODO(flow)` placeholders are left. The agent can always
`flow back` a phase, for example when review finds a problem.

Instead of approving yourself, you can hand spec, tickets or review to a
read-only **judge** (a fresh Claude):
`! flow delegate spec tickets review`. See [The judge](docs/judge.md).

## Commands

| Command | Who | What |
|---|---|---|
| `flow init` | you | opt the repo in |
| `flow status`, `flow check` | anyone | where things are, and what blocks the next phase |
| `flow new <slug> ["<title>"]` | agent | start a feature (one at a time) |
| `flow advance`, `flow back` | agent | move forward or back one phase |
| `flow gates` | agent | in build: run the gates and record the result |
| `flow judge` | agent | in a delegated gate: have the judge decide |
| `flow deploy` | anyone | recent CI runs and the health check |
| `flow approve spec\|tickets\|review` | **you** | approve the current artifact (for review, the code as it is now) |
| `flow delegate <gates…>`, `flow delegate off` | **you** | let the judge pass gates for this feature, or stop it |
| `flow pause`, `flow resume` | **you** | suspend or restore enforcement |
| `flow abandon` | **you** | drop the active feature |

Run your commands with the `!` prefix in Claude Code (`! flow approve spec`) or
from a terminal. `!` commands bypass the hooks, which is why only you can run
them. A slash command can't do the same job, because it runs through the
agent's tools and the hook blocks them.

## Configuration

A minimal `.flow.json`:

```json
{
  "work_dir": "flow",
  "protected_paths": ["lib/", "test/", "mix.exs"],
  "standing_gates": ["mix test"],
  "codegen": ["\\bmix\\s+(phx|ecto)\\.gen\\.\\S"]
}
```

- `protected_paths`: what counts as code.
- `standing_gates`: commands every feature must pass.
- `codegen`: regexes for commands that write code without naming the files.
- `deploy` (optional): hooks up Forgejo, GitHub Actions or a custom command,
  so pushes wait for CI and ship waits for green.

See [Configuration](docs/configuration.md) for every key, the deploy
providers and template overrides.

## Documentation

- [Installing](docs/installing.md): the plugin, the `flow` binary, updates
- [Configuration](docs/configuration.md): `.flow.json`, deploy providers,
  templates, per-repo files
- [Enforcement](docs/enforcement.md): exactly what the hooks check, and how
- [The judge](docs/judge.md): delegating approvals
- [The living spec](docs/living-spec.md): `specs/`, spec changes, adopting
  flow in an existing repo
- [Known limitations](docs/limitations.md)
- [Development](docs/development.md): source layout, tests, releasing
