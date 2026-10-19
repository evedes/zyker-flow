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
sets the protected paths, standing gates and work dir (`work_dir`, `work` by
default; `<work>` below).

| Command | Who | What |
|---|---|---|
| `flow new <slug> "<title>"` | you | start a feature (one active at a time), scaffolds `research.md` |
| `flow status` / `flow check` | you | where things are and what blocks the next phase |
| `flow advance` | you | move on; refuses until `check` passes, scaffolds the next artifact |
| `flow back` | you | return to the previous phase (e.g. review found a problem) |
| `flow gates` | you | in build: run standing gates + every ticket's Verification steps, record the result |
| `flow deploy` | you | CI runs for HEAD and the health check |
| `flow approve spec\|tickets` | **user only** | sign off; stays valid until the file changes |
| `flow pause` / `resume` / `abandon` | **user only** | suspend enforcement, or drop the active feature |

Never run the user-only commands, and never edit `<work>/flow.json` or
`<work>/*/state.json`: the hooks deny both. When you need an approval, say so
and ask the user to run `! flow approve <kind>`.

`.flow.json`, `<work>/templates/` and `<work>/STANDING.md` set the rules. The
hooks don't stop you editing them, but never do it to get past a check during
a feature; propose the change and let the user make it. If a hook denies with
`hook error: …`, don't work around it: tell the user (usually `.flow.json` is
invalid and needs fixing by hand). A `hook error:` about the payload (a lone
surrogate, `expected value at line …`) means flow could not read the tool
call itself: retry it without the unusual characters, and tell the user if
that does not help. `hook error: no flow binary: …` means the
plugin's launcher found no flow binary to run (offline with no cached release,
no cargo to build one): tell the user what it says; every call stays denied
until it is fixed, and a download is retried after five minutes.

## Phases

All artifacts live in `<work>/<NNN-slug>/` and are committed with the feature.
They are scaffolded from the repo's templates in `<work>/templates/` when it
has them, else the plugin's. `TODO(flow)` marks unfilled template text;
`flow check` fails while any remain, or while a section the template seeded
with one is missing or empty.

1. **explore** — fill `research.md`. Read the code before proposing anything;
   use a subagent (Explore) for wide reading so findings come back distilled.
   Put decisions only the user can make under `## Open questions` as `- [ ]`
   items, then stop and ask. Tick them (`- [x]`) with the answer once given.
2. **spec** — fill `spec.md`: the user-visible behaviour, data model, UI and
   testable acceptance criteria, plus what is out of scope. Present it and ask
   for `! flow approve spec`. Changing `spec.md` afterwards voids the approval.
3. **tickets** — split the spec into `tickets/T01-<slug>.md`, `T02-…`:
   `flow advance` scaffolds `tickets/T01.md`; rename it and copy it for each
   further ticket. Every field the standing agreement (`<work>/STANDING.md`)
   doesn't cover must be specific; `## Verification steps` holds the runnable
   commands that prove the ticket (one per line, in a ``` block). Ask for
   `! flow approve tickets`. Leave `spec.md` and the tickets alone once
   approved: `flow gates` and every later phase refuse until they are restored
   or approved again (`flow back` to that phase).
4. **build** — per ticket, write the failing test from its acceptance criteria
   first, then the code, then check it in the running app. When all tickets
   are done, `flow gates` must pass. Any later change to a file git sees
   outside `<work>` makes the gates stale, docs and untracked scratch files
   included: finish those before `flow gates`, and keep scratch output out of
   the repo (or gitignored) from then on.
5. **review** — dispatch a fresh subagent that did not write the code. Give it
   `spec.md`, the tickets and `git diff`, and have it write `review.md`
   (Verdict `pass`/`fail`, findings with file:line). On `fail`: `flow back`,
   fix, `flow gates`, `flow advance`, review again. Every move into review
   keeps the previous `review.md` as `review-<n>.md` and starts a fresh one,
   so each round needs a new review. Change nothing during review, docs
   included: gates run only in build, so any change means going back there.
6. **ship** — commit code and the `<work>` folder together (done refuses
   while the feature's folder has uncommitted changes). With a `deploy`
   section in `.flow.json`, push too: the push hook refuses while a CI/deploy
   run is in flight. Watch with `flow deploy`; `flow advance` to **done** once
   CI is green for HEAD and the health check returns 200. Without one, ship
   ends once everything is committed. Moving to done rewrites `state.json`, so
   commit the `<work>` folder once more afterwards.

Work that touches no protected path (docs, CI, tooling) needs no feature. To
commit it mid-feature, stage it and run a plain `git commit -m '<message>'`
on its own, with a literal message: that is judged by what is staged, while
`-a`, a pathspec, a `git add` in the same call or a `$(…)` message (a heredoc
one included) counts the whole working tree, unstaged code included.
