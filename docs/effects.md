# The effect ledger

Putting files back can't undo what a feature did outside the repo. The one such
effect flow knows of is a deploy: in ship, a push to the deploy branch puts the
feature's code in production, migrations and all. The ledger records, before
that push, what was deployed and how to put it back, and holds a rewind or an
abandon until it is put back or you keep it.

## Recording a deploy

With `record` and `undo` in `.flow.json`'s `deploy` section
([configuration](configuration.md#deploy-providers)):

```json
"deploy": {
  "record": "curl -fsS https://app.example.com/version",
  "undo": "Run the deploy workflow with image_tag={commit}, then roll the database back to {migration}."
}
```

the push hook, in ship, runs `record` before letting the push through and adds
an entry to the feature's `state.json`: when, what `record` printed, and
`undo` with its placeholders filled in (`{before}` is the whole output,
`{<key>}` a field of it when it is a JSON object). If `record` fails, the push
is refused: a deploy that can't be recorded couldn't be undone.

Only the feature's first deploy is recorded: undoing it puts back what was
there before the feature, which undoes every later push too. A push other
hooks refuse records nothing; one that fails after the hook let it through
leaves an entry whose undo is already done, which `flow effects check` finds.

`record` should print what identifies the deployment and nothing that changes
on its own (uptime, a timestamp): the undo is done when it prints the same
again.

## Undoing it

```
flow effects            # the ledger
flow effects --prompt   # the undo steps, newest first, as a prompt for an agent
flow effects check      # reruns each record command; marks what is undone
! flow accept <n>       # keep effect n as it is
```

`--prompt` is for handing to an agent (or following by hand); it ends by
asking for `flow effects check`. flow can't make the agent succeed: what it
guarantees is that nothing counts as undone until flow has run the record
command itself and got back what it printed before (JSON compared as JSON, so
key order and spacing don't matter). `flow abandon` and `flow rewind` refuse
while an entry is neither undone nor kept, naming it.

`flow accept` is yours, like `approve`: it says the deploy stays, and it is
the way out when an undo can't be done. A feature that reaches done keeps its
deploy; the ledger stays in its `state.json` as a record.
