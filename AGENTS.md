# AGENTS.md

This repo (`~/Work/zyker-flow`, GitHub `evedes/zyker-flow`) is the
zyker-flow marketplace. It holds `flow`, a Claude Code plugin: a
phase-gated feature workflow whose order is enforced by hooks. `README.md`
describes what it does, how it works and how repos use it, and lists its
known limitations. `CLAUDE.md` is a symlink to this file.

- The implementation is the Rust crate at the root (`src/`, one binary `flow`;
  `templates/` are compiled in; README's "Files" names each module).
  `bin/flow` is a POSIX sh launcher that runs it with `FLOW_SELF` set to its
  own path: this checkout's `target/release/flow`, rebuilt by cargo when a
  source is newer, else the pinned release binary (README, "The flow binary").
  `hooks/hooks.json` runs the launcher on every Edit/Write/MultiEdit/
  NotebookEdit/Bash/Monitor call and at session start; `skills/flow/SKILL.md` tells
  the agent how to run each phase; `templates/` are the artifact skeletons
  (and decide what `flow check` requires).
- Write plain Rust: serde types, one `Error` with `?`, fancy-regex for codegen
  patterns, ureq for HTTP. The fingerprint and artifact hashes never change:
  features in progress recorded them (tests/flow.rs,
  `fingerprint_and_artifact_hashes_are_stable`, pins them).
- Tests: `cargo test` (tests/flow.rs, tests/launcher.rs, tests/smoke.rs), the
  launcher suite again under another shell (`FLOW_TEST_SH=<absolute path>
  cargo test --test launcher`) and `scripts/smoke`; keep `cargo fmt --check`
  and `cargo clippy --all-targets -- -D warnings` clean (README, "Tests").
  They drive the CLI, the hooks and the launcher end to end against throwaway
  git repos. Add one for every behaviour change, and one for each item under
  README's "Known limitations" you fix (then drop it from that list).
- The plugin is loaded from this directory in place (`claude plugin list
  --json` shows `readFromFolder`), so a saved change here, committed or not,
  is live in every flow repo's next session (the launcher rebuilds
  `target/release/flow` on the next call, inside a hook: keep `cargo build
  --release --locked` passing, or it keeps running the previous build). A
  hook that fails denies every tool call there (`hook error: ...`), so run the
  tests and `scripts/smoke` (the hooks against every `~/Work` repo with a
  `.flow.json`) before leaving `src/` or `bin/flow` in a new state.
- Releases: `VERSION` and the committed `SHA256SUMS` pin the binary everyone
  else runs; bump and pin them only as README's "Releasing" says (tag first,
  `scripts/pin-release`, then main), never by hand.
- `.claude-plugin/plugin.json` has no `version` on purpose: installed from a
  git URL, the version is then the commit, so updates aren't held back by an
  unbumped version string. `VERSION` is the flow binary's release, not the
  plugin's version.
- The user's `~/.local/bin/flow` wrapper execs this checkout's `bin/flow`.
- Repos using it (the ones `scripts/smoke` finds): atomize, zyker and zyker-cloud
  (`~/Work/atomize`, `~/Work/zyker`, `~/Work/zyker-cloud`).
- When a behaviour changes, update `README.md` and `skills/flow/SKILL.md` in
  the same commit: the skill is what the agent reads in every flow repo.
