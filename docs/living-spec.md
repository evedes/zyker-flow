# The living spec

`<work_dir>/specs/<area>.md` says how the system behaves now: one file per
area (lowercase letters, digits, `-` and `_`), one `## <requirement>` section
per requirement, and any text before the first one (a title, an intro) kept as
it is.

## Spec changes

Each feature says what it changes in the living spec under `## Spec changes`
in `spec.md`, so the spec approval covers it:

```markdown
## Spec changes
### Added auth: Password reset
A user who forgot their password can ...

### Changed auth: Login
The full new text of the requirement.

### Removed billing: Legacy invoices
```

Added and Changed carry the requirement's whole text (`####` headings, such as
scenarios, included); Removed needs none, and anything below it is dropped.
Renaming a requirement is a Removed and an Added. Names match whatever their
case and spacing; a Changed entry renames to its own spelling. With nothing to
change, the section says `None.`.

`flow check` in spec, and again in ship, refuses spec changes that would not
apply: an Added requirement the area already has, a Changed or Removed one it
lacks (or has twice), two entries for one requirement, a heading of another
shape, or text with no entries.

## At done

Ship→done merges them: Changed in place, Added at the end of its area (a new
area file starts with `# <area>`), Removed taken out, and the touched files
rewritten with blank lines between headings and bodies. Done then commits them
itself, with the feature's `state.json` and `<work_dir>/flow.json` where the
repo tracks it (`flow: <id> done`, only those paths, the repo's own commit
hooks running). If any step fails, the commit or one before it (a spec file it
can't write, say), done puts every file back, leaves the feature in ship and
says why; a file it can't put back is named, to restore by hand.

Review checks that the merge will be true: `review.md`'s `## Living spec`
(and the judge's review brief) asks whether the code changes behaviour the
living spec describes without an entry in `## Spec changes`, or leaves a
declared change unbuilt; either is a blocking finding.

## Adopting flow in a repo with code

A repo with no code yet grows its living spec from its features' spec
changes. One that adopts flow with code already in it must write the spec
first: while any file git sees under `protected_paths` exists and
`<work_dir>/specs/` holds no `<area>.md` (a file named like an area),
`flow new` refuses, the session-start primer says so, and `flow init` says it
in its next steps.

It is written outside any feature, when the hooks allow editing
`<work_dir>/specs/`, from the code alone:

1. Map the behaviour from the test names.
2. Agree the areas and requirement names with the user.
3. Write one area at a time.
4. Have each checked against the code by a fresh agent.
5. The user reviews it, and it goes in as a docs-only commit.

The skill's "Starting the living spec" has the full steps, so asking Claude to
write it is enough.
