# Rewinding a feature

Sometimes the work went wrong early enough that fixing it costs more than
starting again: the spec chose the wrong approach, the tickets cut it the wrong
way, the code wandered off. `flow back` keeps everything and moves one phase;
`flow rewind` discards the work done since a phase began and puts the feature
back at its start.

```
! flow rewind            # list the checkpoints
! flow rewind tickets    # back to how things were on entering tickets
```

It is the user's command, like `approve` and `abandon`: the agent can propose
it, not run it.

## Checkpoints

Each time the feature enters a phase, `flow new` (explore) and `flow advance`
(spec, tickets, build, review) save a checkpoint: a commit of every file git
sees, staged, changed or untracked (not ignored), under
`refs/flow/<id>/<phase>`. It hangs off HEAD but sits outside every branch, so
it never shows up in `git log` or a push, and the index and working tree are
left as they were. It is taken after the phase's own setup, so the spec
checkpoint already has the scaffolded `spec.md`. Entering a phase again
replaces its checkpoint; `flow back` takes none.

A feature started before checkpoints existed has none for the phases it entered
earlier, and can't be rewound to them.

## What a rewind does

`flow rewind <phase>` (explore, spec, tickets, build or review; the current
phase or an earlier one):

1. Saves the work as it is now as `refs/flow/<id>/attempt-<n>`, so nothing is
   lost: `git diff refs/flow/<id>/attempt-1` shows it, and
   `git restore --source=refs/flow/<id>/attempt-1 -- <path>` takes a file back.
2. Puts the feature's folder and the protected paths back to the checkpoint:
   files changed since are restored, files added since are deleted, and
   anything staged there is unstaged. `state.json` is not restored, and files
   outside those paths (docs, CI, tooling) are left alone.
3. Moves the feature to that phase, drops the approvals of that phase and later
   ones, and, rewinding to build or earlier, the gate result. The history
   records the rewind and the attempt it kept. Judge verdicts stay: if the same
   artifact comes back, `flow judge` gives its approval back without asking
   again.

It refuses when protected code was committed since the checkpoint (in ship,
say): putting files back doesn't undo a commit. That is also why ship can't be
rewound to. It refuses too while the feature has a deploy neither undone nor
kept ([the effect ledger](effects.md)).

Rewinding to explore is starting over: research, spec, tickets and code all go
back to the feature's first moment, with the attempt kept.

## Reset commands

Putting files back doesn't touch what the discarded work did outside them: a
dev database it migrated, test data it created. `.flow.json`'s `reset` lists
shell commands that put that environment back to match the code:

```json
"reset": ["bin/dev run --rm -T app mix ecto.reset"]
```

`flow rewind` runs them after it has put the files back, so they rebuild from
the code as the checkpoint had it. They run at the git root with `/bin/sh`, in
order, stdin closed, and stop at the first that fails; the rewind stays done,
exits 1, and says so. `flow reset` runs them again on their own (the agent may
too: they only touch the disposable environment). `flow abandon` doesn't run
them, since it leaves the code in place.

Keep them to what is disposable and local. A reset can name another repo's
environment when the two share state: when an app's dev database also runs a
library repo's migrations, the library's rewind may want
`(cd ../app && bin/dev run --rm -T app mix ecto.reset)` as well.
