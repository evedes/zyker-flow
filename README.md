<div align="center">

<img src=".github/flow.svg?v=2" alt="FLOW" width="468">

**`flow`** — a phase-gated feature workflow for Claude Code.

*enforced by hooks, not by prompts: the agent can't skip a step.*

<br>

[![CI](https://github.com/evedes/zyker-flow/actions/workflows/ci.yml/badge.svg)](https://github.com/evedes/zyker-flow/actions/workflows/ci.yml)

![Claude Code](https://img.shields.io/badge/claude_code-plugin-000?style=for-the-badge&logo=anthropic&logoColor=white)
![Rust](https://img.shields.io/badge/rust-1.85-000?style=for-the-badge&logo=rust&logoColor=white)
![Linux](https://img.shields.io/badge/linux-x86__64_·_aarch64-000?style=for-the-badge&logo=linux&logoColor=white)
![macOS](https://img.shields.io/badge/macos-x86__64_·_aarch64-000?style=for-the-badge&logo=apple&logoColor=white)
![POSIX](https://img.shields.io/badge/posix-sh-000?style=for-the-badge&logo=gnubash&logoColor=white)

</div>

---

## `> the problem`

Tell a coding agent to research first, write a spec, get it approved, test,
then commit, and it will, until it doesn't. Halfway through a long session,
after a compaction, or on a change that looks like one line, it starts
writing code before anyone agreed on what to build, or commits work that was
never reviewed. Instructions in a prompt are suggestions. You only find out
it skipped a step when you read the diff.

flow turns the process into rules the agent can't skip.

---

## `> what it is`

Every feature moves through the same phases, in order:

```
explore -> spec -> tickets -> build -> review -> ship -> done
```

The `flow` CLI owns the state, and a hook checks every edit and shell command
against it.

Repos opt in with a `.flow.json` at the git root. Everywhere else the hooks do
nothing.

---

## `> what you get`

- **Code changes only in build.** Files under your `protected_paths` can't be
  edited, overwritten from the shell or regenerated in any other phase.
- **Commits only in ship.** A `git commit` goes through only once the spec and
  tickets are approved and both the gates and the review have passed, all on
  the exact code being committed (a content fingerprint).
- **Approvals stick to what was approved.** An approval is bound to the
  artifact's content, so editing an approved spec or ticket voids it. What
  rests on it goes too: tickets approved on an earlier spec need approving
  again, and gates passed on earlier tickets need rerunning.
- **The rules stay fixed.** While a feature is active, the agent can't edit
  `.flow.json`, the Claude settings, the standing agreement, the templates or
  the living spec.
- **You keep the keys.** Only you can approve, pause, resume, rewind or abandon, and
  the agent can't edit flow's state files. You can hand the spec, tickets or
  review approval to a read-only [judge](#-the-judge), and take it back at any
  time.
- **A living spec.** `<work_dir>/specs/` describes how the system behaves now.
  Each feature declares what it changes in it, and flow merges those changes
  when the feature is done.
- **Fails closed.** If the hook breaks, it denies the call rather than letting
  it through.

Changes that touch no protected path (docs, CI, tooling) are never blocked.

> **not a sandbox.** flow keeps a *cooperative* agent in phase. Anything the
> agent can run can get past a pattern match, and approvals are content
> hashes, not signatures. Hooks can't judge quality either, so a thin spec can
> still pass the checks; approvals and the fresh-context review cover that.
> See [known limitations](docs/limitations.md).

---

## `> boot sequence`

Needs git and a POSIX sh. On Linux and macOS the release binary is fetched on
first use; elsewhere it builds with cargo ([installing](docs/installing.md)).

```sh
git clone https://github.com/evedes/zyker-flow.git ~/zyker-flow
claude plugin marketplace add ~/zyker-flow     # once per machine
claude plugin install flow@zyker-flow
```

Put `flow` on your PATH, for terminals and Claude Code's `!` prefix:

```sh
#!/bin/sh
# ~/.local/bin/flow
exec "$HOME/zyker-flow/bin/flow" "$@"
```

Opt a repo in, at its root:

```sh
flow init        # .flow.json · flow/STANDING.md · permission deny rules
```

`flow init` guesses the settings for Elixir, Rust, Python or Node. Check
`.flow.json` and `flow/STANDING.md` (your standing Definition of Ready/Done),
then commit them. If the repo already has code, ask Claude to write the
[living spec](docs/living-spec.md) before the first feature.

Then work as usual. Ask Claude for a feature: it runs `flow new`, works through
the phases, and stops when it needs you:

```
! flow approve spec
! flow approve tickets
```

---

## `> the phases`

<p align="center">
  <img src=".github/phases.svg?v=2" alt="explore → spec → tickets → build → review → ship → done; review can go back to build" width="880">
</p>

| phase       | the agent                                                              | moves on when                                     |
|-------------|------------------------------------------------------------------------|---------------------------------------------------|
| **explore** | reads the code and living spec, writes `research.md`                   | its open questions are answered                   |
| **spec**    | writes `spec.md`: behaviour, acceptance criteria, living-spec changes  | you run `! flow approve spec`                     |
| **tickets** | splits the spec into `tickets/T01-….md`, each with verification commands | you run `! flow approve tickets`                |
| **build**   | tests first, then code; the only phase where code can change           | `flow gates` passes (standing gates + ticket checks) |
| **review**  | a fresh subagent reviews the diff against the spec, in `review.md`     | the verdict is `pass`                             |
| **ship**    | commits, and pushes if a deploy is configured                          | CI is green and the health check passes, or, with no deploy, everything is committed |
| **done**    | flow merges the spec changes into the living spec and commits them     | —                                                 |

A phase can't be left until its artifact is complete: every required section
is filled in and no `TODO(flow)` placeholders are left. The agent can always
`flow back` a phase, for example when review finds a problem. When starting
again beats fixing, you can `! flow rewind <phase>`: the feature's files and
code go back to how they were on entering that phase, and the discarded attempt
is kept in git ([more on rewinding](docs/rewind.md)).

---

## `> the judge`

You don't have to approve every gate yourself. Hand spec, tickets or review to
a judge, a fresh, read-only Claude that sees only the artifacts:

```
! flow delegate spec tickets review     # for the active feature
! flow delegate off                     # take it back
```

The judge approves, rejects with findings, or asks you a question. Its
approval binds to the same content hash yours would. It doesn't take earlier
phases on trust: when the work is wrong because the research, spec or tickets
under it are, its finding says so (`upstream:`), and the agent goes back to
fix it there. After three rejections the gate comes back to you, and
`flow status` says so. Your own `flow approve` keeps working either way.
[More on the judge](docs/judge.md).

---

## `> commands`

| command                                   | who        | what                                                          |
|-------------------------------------------|------------|---------------------------------------------------------------|
| `flow init`                               | you        | opt the repo in                                               |
| `flow status` · `flow check`              | anyone     | where things are, and what blocks the next phase              |
| `flow new <slug> ["<title>"]`             | agent      | start a feature (one at a time)                               |
| `flow advance` · `flow back`              | agent      | move forward or back one phase                                |
| `flow gates`                              | agent      | in build: run the gates and record the result                 |
| `flow judge`                              | agent      | in a delegated gate: have the judge decide                    |
| `flow deploy`                             | anyone     | recent CI runs and the health check                           |
| `flow approve spec\|tickets\|review`      | **you**    | approve the current artifact (for review, the code as it is now) |
| `flow delegate <gates…>` · `off`          | **you**    | let the judge pass gates for this feature, or stop it         |
| `flow pause` · `flow resume`              | **you**    | suspend or restore enforcement                                |
| `flow rewind [<phase>]`                   | **you**    | discard the work since a phase began (no phase: list checkpoints) |
| `flow reset`                              | anyone     | run `.flow.json`'s reset commands (rewind runs them too)      |
| `flow abandon`                            | **you**    | drop the active feature                                       |

Run yours with the `!` prefix in Claude Code (`! flow approve spec`) or from a
terminal. `!` commands bypass the hooks, which is why only you can run them. A
slash command can't do the same job, because it runs through the agent's tools
and the hook blocks them.

---

## `> config`

A minimal `.flow.json`:

```json
{
  "work_dir": "flow",
  "protected_paths": ["lib/", "test/", "mix.exs"],
  "standing_gates": ["mix test"],
  "codegen": ["\\bmix\\s+(phx|ecto)\\.gen\\.\\S"]
}
```

| key               | signal                                                                 |
|-------------------|------------------------------------------------------------------------|
| `protected_paths` | what counts as code                                                    |
| `standing_gates`  | commands every feature must pass                                       |
| `codegen`         | regexes for commands that write code without naming the files          |
| `deploy`          | optional: Forgejo, GitHub Actions or a custom command; pushes wait for running CI, ship waits for green |
| `judge`           | optional: the judge's model and budget                                 |

Every key, the deploy providers and template overrides:
[configuration](docs/configuration.md).

---

## `> docs`

| page                                       | what's in it                                              |
|--------------------------------------------|-----------------------------------------------------------|
| [installing](docs/installing.md)           | the plugin, the `flow` binary, updates                    |
| [configuration](docs/configuration.md)     | `.flow.json`, deploy providers, templates, per-repo files |
| [enforcement](docs/enforcement.md)         | exactly what the hooks check, and how                     |
| [the judge](docs/judge.md)                 | delegating approvals                                      |
| [the living spec](docs/living-spec.md)     | `specs/`, spec changes, adopting flow in an existing repo |
| [rewinding](docs/rewind.md)                | checkpoints, and discarding work to start a phase again   |
| [known limitations](docs/limitations.md)   | what flow doesn't catch yet                               |
| [development](docs/development.md)         | source layout, tests, releasing                           |

---

<div align="center">

<sub>zyker-flow · made by <a href="https://github.com/evedes">@evedes</a></sub>

🇵🇹 &nbsp; 🇪🇺

</div>
