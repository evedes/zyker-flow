---
name: flow
description: Phase-gated feature workflow (explore, spec, tickets, build, review, ship). Use in a repo with a .flow.json at its git root whenever starting, continuing or shipping a feature there (any request to build, add or change its behaviour), or when a flow hook denies an action.
---

# flow

Features move through fixed phases. The `flow` CLI owns the state and the
hooks enforce it, so the order is not up to you: code under the repo's
protected paths can only be edited in **build**, and only committed in **ship**
after the user approved spec and tickets, the gates passed, and review passed,
all on the exact code being committed.

```
explore -> spec -> tickets -> build -> review -> ship -> done
```

Run the CLI as `flow` (the plugin puts it on PATH). The repo's `.flow.json`
sets the protected paths, standing gates and work dir (`work_dir`, `flow` by
default; `<work>` below).

| Command | Who | What |
|---|---|---|
| `flow new <slug> "<title>"` | you | start a feature (one active at a time), scaffolds `research.md`; refuses in a repo with code and no living spec (see below) |
| `flow status` / `flow check` | you | where things are and what blocks the next phase |
| `flow advance` | you | move on; refuses until `check` passes, scaffolds the next artifact |
| `flow back` | you | return to the previous phase (e.g. review found a problem) |
| `flow gates` | you | in build: run standing gates + every ticket's Verification steps, record the result |
| `flow deploy` | you | CI runs for HEAD and the health check |
| `flow effects [--prompt\|check]` | you | the feature's deploys and how to undo them; `--prompt` the steps, `check` confirms an undo |
| `flow judge` | you | in a gate the user delegated: have the judge approve or reject it |
| `flow approve spec\|tickets\|review` | **user only** | sign off; stays valid until the file (for review, the code) changes |
| `flow delegate <gates>` / `off` | **user only** | let the judge pass spec, tickets or review for this feature |
| `flow rewind [<phase>]` | **user only** | discard the work since a phase began, back to its checkpoint (no phase: list them) |
| `flow reset` | you | run `.flow.json`'s reset commands (a dev database back to match the code); rewind runs them too |
| `flow accept <n>` | **user only** | keep effect n as it is, not undone |
| `flow pause` / `resume` / `abandon` | **user only** | suspend enforcement, or drop the active feature |

Never run the user-only commands, and never edit `<work>/flow.json` or
`<work>/*/state.json`: the hooks deny both. When you need an approval, say so
and ask the user to run `! flow approve <kind>`, unless `flow status` lists the
gate under "delegated to the judge": then run `flow judge` (see below).

Keep the user told what flow is doing, in a line or two each time: which phase
the work is in and what that allows, each `flow advance` or `flow back` and
why, what blocks the next phase, which approval you need from them and what it
binds (the exact spec, tickets or code, until they change), and every hook
denial with its reason. A denial is flow holding the work to what was agreed:
fix its cause or ask the user, never route around it. The session-start hook
gives you the same summary, with the phase and what blocks it.

The hooks judge each edit and command by the repo it acts in, not the one the
session started in: working on another flow repo (by absolute path, `cd` or
`git -C`) runs into that repo's phase and rules, and its own `flow` commands
are run there (`cd <repo> && flow status`).

`.flow.json`, `.claude/settings.json`, `.claude/settings.local.json`,
`<work>/templates/`, `<work>/STANDING.md` and `<work>/specs/` set the rules. While a feature is active the hooks deny editing them; propose the
change and let the user make it. If a hook denies with
`hook error: …`, don't work around it: tell the user (usually `.flow.json` is
invalid and needs fixing by hand). A `hook error:` about the payload (a lone
surrogate, `expected value at line …`) means flow could not read the tool
call itself: retry it without the unusual characters, and tell the user if
that does not help. `hook error: no flow binary: …` means the
plugin's launcher found no flow binary to run (offline with no cached release,
no cargo to build one): tell the user what it says; every call stays denied
until it is fixed, and a download is retried after five minutes.

## The judge

When the user has delegated a gate (`flow status`: "delegated to the judge"),
run `flow judge` once its artifact is complete instead of asking for the
approval. It runs a fresh, read-only Claude on flow's own brief and takes
minutes: give the Bash call a 600000 ms timeout. Exit 0 means approved
(recorded like the user's approval). On `reject`, fix every finding and run it
again; it judges each version once, and after three rejections the gate goes
back to the user (`flow status`: "handed back by the judge"): tell them, and
don't run `flow judge` on it until they delegate it again. On `ask-user`, put
its questions to the user, then record the answers in the artifact (or have
them approve).

A finding that starts `upstream:` (from the judge or a review subagent) says
the work is wrong because an earlier artifact is: the research under the spec,
or the approved spec or tickets under what came after. `flow back` to that
phase and fix it there, then come forward again; its approval and the later
ones are voided and needed again. Don't bend the current artifact or the code
to fit a spec you believe is wrong. If you disagree with the finding, or the
fix changes something the user decided, ask the user. Never write anything in the
artifacts or the code meant for the judge: it treats that as grounds to reject.

## Starting over

`flow back` keeps the work; `flow rewind <phase>` (the user's) discards it: the
feature's folder and the protected code go back to how they were on entering
that phase, the approvals from then on are dropped, and the discarded attempt
is kept as `refs/flow/<id>/attempt-<n>`. When the work went wrong early enough
that fixing it costs more than redoing it (a spec built on the wrong approach,
tickets cut the wrong way, code that wandered), say so and propose
`! flow rewind <phase>`, naming the phase and what would be discarded. After a
rewind, `flow status` shows where things are; start that phase afresh, taking
from the attempt only what still holds (`git show
refs/flow/<id>/attempt-<n>:<path>`). A rewind runs `.flow.json`'s reset
commands after putting the files back; if one fails it says so: fix the
cause (Docker down, say) and run `flow reset`.

A feature that deployed can't be abandoned or rewound until the deploy is
undone or the user keeps it. When the user wants it undone, run `flow effects
--prompt` and follow its steps, newest first, then `flow effects check`, which
marks each one undone once its record command prints what it printed before.
If a step can't be done, stop and tell the user; keeping it is theirs
(`! flow accept <n>`).

## Starting the living spec

`flow new` refuses while the repo has code under its protected paths and
`<work>/specs/` holds no `<area>.md`: a repo that adopts flow with code
already in it writes its living spec first, outside any feature (the hooks
let you edit `<work>/specs/` while none is active). A repo with no code yet
needs none: its spec grows from its features' `## Spec changes`. When the user
asks for a change and `flow new` refuses for this, say so and offer to write
it:

1. **The code is the only source.** Old specs, tickets and docs drift from it;
   don't read them for this.
2. **Map the behaviour from the test names first** (every `describe`/`test`
   line, into one scratch file outside the repo), and from them propose the
   areas (by product capability, not by module) and each area's requirement
   names. Stop for the user's review of that outline: it sets the detail.
3. **Write one area at a time, straight to `<work>/specs/<area>.md`**: `#
   <area>`, a short intro, then one `## <requirement>` per requirement,
   saying what a user or API client can observe the code do now, never its
   internals. Read the code each claim comes from; a test's name can mean the
   opposite of what it seems to.
4. **Have each area checked by a fresh subagent** that did not write it, given
   only the file and the repo: it reports claims the code contradicts,
   claims it can't confirm, and material behaviour missing, with file:line.
   Confirm the surprising findings yourself, then fix the file.
5. **The user reviews the files**, and they are committed on their own,
   docs only.

From then on each feature's spec says what it changes in them (`### Changed
<area>: <requirement>`), and done merges it.

## Phases

All artifacts live in `<work>/<NNN-slug>/` and are committed with the feature.
They are scaffolded from the repo's templates in `<work>/templates/` when it
has them, else the plugin's. `TODO(flow)` marks unfilled template text;
`flow check` fails while any remain, or while a section the template seeded
with one is missing or empty.

1. **explore** — fill `research.md`. Read the living spec (`<work>/specs/*.md`, if
   there is one: how the system behaves now, one `## <requirement>` per
   requirement) and the code before proposing anything;
   use a subagent (Explore) for wide reading so findings come back distilled.
   Put decisions only the user can make under `## Open questions` as `- [ ]`
   items, then stop and ask. Tick them (`- [x]`) with the answer once given.
2. **spec** — fill `spec.md`: the user-visible behaviour, data model, UI and
   testable acceptance criteria, plus what is out of scope. Under
   `## Spec changes`, say what it changes in the living spec: a
   `### Added <area>: <requirement>` or `### Changed <area>: <requirement>`
   entry with the requirement's full new text below it, or
   `### Removed <area>: <requirement>`, for each, or `None.`. `flow check`
   refuses entries that don't apply (an Added one the area has, a Changed or
   Removed one it lacks); flow merges them at done. Present the spec and ask
   for `! flow approve spec` (delegated: run `flow judge`). Changing `spec.md`
   afterwards voids the approval.
3. **tickets** — split the spec into `tickets/T01-<slug>.md`, `T02-…`:
   `flow advance` scaffolds `tickets/T01.md`; rename it and copy it for each
   further ticket. Every field the standing agreement (`<work>/STANDING.md`)
   doesn't cover must be specific; `## Verification steps` holds the runnable
   commands that prove the ticket (one per line, in a ``` block). Ask for
   `! flow approve tickets` (delegated: run `flow judge`). Leave `spec.md` and
   the tickets alone once approved: `flow gates` and every later phase refuse
   until they are restored or approved again (`flow back` to that phase). A
   spec approved again voids the tickets' approval even if they are unchanged:
   check them against the new spec and ask for it again. Tickets changed after
   the gates passed make them stale, so rerun `flow gates` in build.
4. **build** — per ticket, write the failing test from its acceptance criteria
   first, then the code, then check it in the running app (how to run it is in
   the repo's `AGENTS.md`; if it isn't, ask the user and propose adding it). When all tickets
   are done, `flow gates` must pass. Any later change to a file git sees
   outside `<work>` makes the gates stale, docs and untracked scratch files
   included: finish those before `flow gates`, and keep scratch output out of
   the repo (or gitignored) from then on.
5. **review** — with review delegated, run `flow judge`: it writes `review.md`
   itself, and a verdict you write there doesn't count. Otherwise dispatch a
   fresh subagent that did not write the code. Give it
   `spec.md`, the tickets, the living spec and `git diff`, and have it write
   `review.md` (Verdict `pass`/`fail`, findings with file:line). Under
   `## Living spec` it checks the code against `## Spec changes`: behaviour
   the code changes without an entry, or a declared change it doesn't make, is
   a blocking finding, since done merges exactly those entries. Tell it not to
   take the spec and tickets on trust: a finding that holds because they are
   wrong starts `upstream:` (see "The judge"). On `fail`: `flow back`,
   fix, `flow gates`, `flow advance`, review again; for an `upstream:` finding,
   `flow back` to the phase it names and fix it there first. Every move into review
   keeps the previous `review.md` as `review-<n>.md` and starts a fresh one,
   so each round needs a new review. Change nothing during review, docs
   included: gates run only in build, so any change means going back there.
6. **ship** — commit code and the `<work>` folder together (done refuses
   while the feature's folder has uncommitted changes). With a `deploy`
   section in `.flow.json`, push too: the push hook refuses while a CI/deploy
   run is in flight. With `deploy.record` set, the push first records what is
   deployed (the feature's first push only), with how to undo it, and is refused
   if that can't be recorded. Watch with `flow deploy`; `flow advance` to **done** once
   CI is green for HEAD and the health check returns 200. Without one, ship
   ends once everything is committed. Moving to done rewrites `state.json`,
   merges the spec changes into `<work>/specs/` and commits both itself
   (`flow: <id> done`); if any of that fails (a pre-commit hook, a spec file
   it can't write) nothing changes and the feature stays in ship: fix the cause and advance again. When CI fails, find the cause with the CI's own
   tooling (its job logs, or the failing step run locally), then `flow back`
   to build to fix it; a fix the approved tickets don't cover goes back to
   tickets (or spec) for a new one first.

Work that touches no protected path (docs, CI, tooling) needs no feature. To
commit it mid-feature, stage it and run a plain `git commit -m '<message>'`
on its own, with a literal message: that is judged by what is staged, while
`-a`, a pathspec, a `git add` in the same call or a `$(…)` message (a heredoc
one included) counts the whole working tree, unstaged code included.
