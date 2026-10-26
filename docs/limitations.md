# Known limitations

Open defects; each needs a fix and a test.

## Enforcement

- With no feature active, the agent can edit `.flow.json`,
  `<work_dir>/templates/`, `STANDING.md` and `<work_dir>/specs/`: rules changed
  before `flow new` (and before a `flow delegate`) are the ones the feature runs
  under.
- `flow judge` runs the `claude` on the agent's PATH, so an agent that changes
  PATH can put its own judge there. The judge also reads what the agent wrote,
  and a model can be talked round where a hash can't: a delegated gate is only as
  strong as the judge.
- `flow gates` fingerprints the tree after the commands run, so a gate that
  rewrites code (an unchecked formatter, a generator) passes on code it never
  checked.
- Without a `deploy` section, ship→done only checks that the tree is clean,
  not that the reviewed code is what was committed.
- Only `git commit` (and `commit-tree`) is gated; `merge`, `cherry-pick`,
  `revert`, `am`, `rebase` and `pull` are not, nor a commit through a git alias
  (`git ci`, `git -c alias.x=commit x`).
- Bash write detection misses `&>`/`&>>` and `>|` redirects, git commands that
  rewrite code without naming it (`git restore .`, `git reset --hard`), and
  `cd dir && …` into a protected path, resolves relative paths against the git
  root rather than the shell's cwd, and denies copies *out of* protected paths
  (`cp lib/x /tmp/`).
- Bash commands are placed in a repo by a plain `cd`/`pushd` and `git -C` only:
  a `cd` in a subshell (`(cd ../b && …)`) or through a variable, and git's
  `--git-dir`/`--work-tree`, aren't followed, so those commands are judged where
  the call started.
- The feature's state is untracked until it is first committed, and the hooks
  allow `git stash -u` and `git clean`, which remove it. flow then reports no
  active feature, or, when a tracked `<work_dir>/flow.json` survives, that the
  active feature's state is missing, and denies code edits until it is restored
  or the user runs `flow abandon`.

## The binary and its launcher

- Release binaries exist only for Linux and macOS on x86_64 and aarch64; other
  Unix systems (the BSDs, other architectures) need cargo. flow does not build
  on Windows: it uses Unix APIs and runs gates and `status_command` with
  `/bin/sh`.
- When no release matches the checkout (before the first release, unreleased
  changes to `src/`, a dev checkout without `target/`), the first hook call
  builds flow with cargo inside the hook, about 40 s; the call waits, and is
  blocked if the hook times out.
- Input that is not UTF-8 (arguments, paths, symlink targets, hook stdin, file
  contents) is an error: the CLI exits 1, and pre-tool-use denies in a flow
  repo.
- The launcher compares mtimes with `test -nt`, which macOS's `/bin/sh` (bash
  3.2) does to the second: a source, `VERSION` or `SHA256SUMS` changed within
  the same second as the last build or check is noticed only once something
  changes again.
