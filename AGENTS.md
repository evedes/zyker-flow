# AGENTS.md

This repo (`~/Work/zyker-flow`, GitHub `evedes/zyker-flow`) is the
zyker-flow marketplace. It holds `flow`, a Claude Code plugin: a
phase-gated feature workflow whose order is enforced by hooks. `README.md`
describes what it does, how it works and how repos use it, and lists its
known limitations. `CLAUDE.md` is a symlink to this file.

- `bin/flow` is the whole implementation: Python 3.8+, stdlib only, one file.
  `hooks/hooks.json` runs it on every Edit/Write/MultiEdit/NotebookEdit/Bash
  call and at session start; `skills/flow/SKILL.md` tells the agent how to run
  each phase; `templates/` are the artifact skeletons (and decide what
  `flow check` requires).
- Tests: `python3 -m unittest discover -s tests`. They drive the CLI and the
  hooks end to end against throwaway git repos. Add one for every behaviour
  change, and one for each item under README's "Known limitations" you fix
  (then drop it from that list).
- The plugin is loaded from this directory in place (`claude plugin list
  --json` shows `readFromFolder`), so a saved change here, committed or not,
  is live in every flow repo's next session. A hook that raises denies every
  tool call there (`hook error: ...`), so run the tests and `scripts/smoke`
  (the hooks against every `~/Work` repo with a `.flow.json`) before leaving
  `bin/flow` in a new state.
- `.claude-plugin/plugin.json` has no `version` on purpose: installed from a
  git URL, the version is then the commit, so updates aren't held back by an
  unbumped version string.
- The user's `~/.local/bin/flow` wrapper execs this checkout's `bin/flow`.
- Repos using it (the ones `scripts/smoke` finds): atomize (`~/Work/atomize`).
- When a behaviour changes, update `README.md` and `skills/flow/SKILL.md` in
  the same commit: the skill is what the agent reads in every flow repo.
