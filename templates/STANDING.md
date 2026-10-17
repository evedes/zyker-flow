# Standing agreement for {project} tickets

Every ticket in `{work_dir}/*/tickets/` inherits this. Tickets only state what is
specific to them.

## Definition of Ready (`flow check` enforces the ticket sections)

- Title and desired end state are specific.
- Context says why, and which spec section the ticket implements.
- Entry points name the files/modules to start from.
- In scope and out of scope are stated.
- `## Verification steps` holds runnable commands.
- The spec and ticket set are approved by the user (`flow approve`).

## Definition of Done

- The ticket's gate holds and its verification commands pass.
- Standing gates pass (`flow gates` runs them; they are listed in `.flow.json`).
- Tests were written before the code they cover, from the acceptance criteria.
- Behaviour was checked in the running app.
- No out-of-scope surfaces touched; existing conventions followed.
- Lands as one commit with its `{work_dir}/` folder.

## Working agreement

- On real ambiguity, stop and ask; don't guess at behaviour the spec doesn't cover.
- Note any deviation from a ticket in `review.md`.
