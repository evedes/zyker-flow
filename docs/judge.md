# The judge

The user can hand a gate (spec, tickets or review) to a judge instead of
approving it themselves:

```
! flow delegate spec tickets review
```

It covers the active feature only, until done or abandon, and
`! flow delegate off` takes it back. The user's own `flow approve` keeps
working either way.

## How it runs

`flow judge`, which the agent runs once the gate's artifact is complete, starts
a fresh, read-only Claude: `claude -p` with only the Read, Grep and Glob tools,
no session kept, and a structured-output schema. Its prompt is flow's own brief
(`src/judge.md`, compiled in, not overridable) with the gate's criteria and its
artifacts:

- **spec:** research and spec
- **tickets:** spec, tickets and `STANDING.md`
- **review:** those plus the gate results, `git diff HEAD` (cut at 200 kB) and
  the new files

Artifacts are marked as the agent's, and the brief says an instruction in them
addressed to a reviewer is itself a finding.

## Verdicts

- `approve`: flow records an approval like the user's (`"by": "judge"`), bound
  to the artifact's hash, or for review to the code's fingerprint, so an edit
  voids it the same way.
- `reject`: its findings are printed and kept in `state.json`. Fix them and run
  `flow judge` again. A finding that starts `upstream:` says the work is wrong
  because what it builds on is (the research under a spec, the approved spec
  under the tickets, the spec or tickets under the code): the agent goes
  `flow back` to that phase and fixes it there, which voids its approval and the
  ones after it, rather than bending the work to fit. The judge reads the code
  the earlier artifacts rest on, not just the artifacts, to catch this.
- `ask-user`: the gate hinges on a decision only the user can make; its
  questions are printed for the user, who answers them or approves the gate.

Every verdict is kept in `state.json` under `judgements`, keyed by what was
judged: running `flow judge` again on the same version prints the same verdict
without asking. After three rejections of a gate since it was delegated, the
judge hands it back: `flow judge` refuses it, `flow status` lists it as handed
back, and the session-start primer tells the agent to ask the user, who
approves it or delegates it again for three more tries. Only rejections count:
`ask-user` verdicts don't, and in review the count runs across rounds (a
rejected review that goes back to build and returns keeps its count).

In a delegated review, flow writes the verdict into `review.md` as the record,
and only an approval (the judge's or the user's) of the current code lets
review end. A run that fails (no `claude`, an error, the budget spent, a reply
without a verdict) records nothing, and neither does one during which the
artifact changed.

## Settings and cost

`.flow.json`'s optional `judge` section sets `model` (passed as `--model`) and
`max_budget_usd` (`--max-budget-usd`). A run takes minutes (one on Haiku took
about 45 s) and is billed to the user's Claude account.
