# AGENTS.md

agile-flywheel holds `flow`, a Claude Code plugin: a phase-gated feature
workflow whose order is enforced by hooks. `README.md` describes what it does
and how repos use it. `CLAUDE.md` is a symlink to this file.

- `bin/flow` is the whole implementation: Python 3, stdlib only, one file.
  `hooks/hooks.json` runs it on every Edit/Write/Bash call and at session
  start; `skills/flow/SKILL.md` tells the agent how to run each phase;
  `templates/` are the artifact skeletons (and decide what `flow check` requires).
- Tests: `python3 -m unittest discover -s tests`. They drive the CLI and the
  hooks end to end against throwaway git repos. Add one for every behaviour change.
- The plugin is installed from this directory in place, so a change here is
  live in every flow repo's next session. A hook that raises denies every
  tool call there (`hook error: ...`), so run the tests and `scripts/smoke`
  (the hooks against every `~/Work` repo with a `.flow.json`) before leaving
  `bin/flow` in a new state.
- `.claude-plugin/plugin.json` has no `version` on purpose: installed from a
  git URL, the version is then the commit, so updates aren't held back by an
  unbumped version string.
- Repos using it: zyker (`~/Work/zyker`).
