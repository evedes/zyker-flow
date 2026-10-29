# Development

Working on flow itself. `AGENTS.md` has the conventions an agent follows here.

## Files

| Path | What |
|---|---|
| `.claude-plugin/plugin.json` | plugin manifest (`flow`; no `version`, see `AGENTS.md`) |
| `.claude-plugin/marketplace.json` | the one-plugin marketplace `zyker-flow` (`source: "."`) |
| `bin/flow` | the launcher (POSIX sh): finds or fetches the flow binary and runs it (see [The flow binary](installing.md#the-flow-binary)) |
| `src/`, `Cargo.toml`, `Cargo.lock` | the CLI and both hooks, in Rust (one binary, `flow`; `templates/` are compiled in): `main.rs` dispatches, `commands.rs` and `hooks.rs` are the entry points, `checks.rs` what each phase needs, `shell.rs` reads Bash commands, `markdown.rs` the artifacts, `specs.rs` the living spec, `checkpoint.rs` checkpoints and `flow rewind`, `judge.rs` `flow judge` (its brief in `judge.md`), `config.rs` the JSON files, `repo.rs` the repo and its fingerprints, `deploy.rs` CI runs and the health check, `primer.rs` what the session-start hook tells the agent, `paths.rs` and `error.rs` the rest |
| `rustfmt.toml` | `cargo fmt` settings |
| `.github/workflows/` | `ci.yml` (fmt, clippy and every test suite on Linux and macOS; the release pin on main) and `release.yml` (the four release binaries) |
| `VERSION`, `SHA256SUMS` | the release bin/flow runs, and its pinned checksums (see [Releasing](#releasing)) |
| `hooks/hooks.json` | `PreToolUse` on `Edit\|Write\|MultiEdit\|NotebookEdit\|Bash\|Monitor`, and `SessionStart` |
| `skills/flow/SKILL.md` | how the agent runs each phase |
| `templates/` | artifact skeletons; they decide what `flow check` requires |
| `docs/` | the user documentation the README links to |
| `tests/` | end-to-end tests against throwaway git repos: `flow.rs` (the CLI and hooks), `launcher.rs` (bin/flow), `smoke.rs` (the smoke check, and its `real_repos` run) |
| `scripts/smoke` | runs `tests/smoke.rs` against the real repos that use flow (see [Tests](#tests)) |
| `scripts/source-hash`, `scripts/pin-release` | the sources' hash a release records, and pinning a published release |

## Tests

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo build --release --locked
cargo test                                   # tests/flow.rs, launcher.rs, smoke.rs, unit tests
FLOW_TEST_SH=$(command -v zsh) cargo test --test launcher   # the launcher under another shell
scripts/smoke
```

`FLOW_TEST_SH` must be an absolute path; CI runs the launcher suite again
under bash on Linux and zsh on macOS. `FLOW_TEST_BIN=<absolute path> cargo
test --test flow` runs `tests/flow.rs` against another binary, such as
`$PWD/bin/flow`.

A plugin loaded from a local checkout is live in every repo as soon as a
change is saved, so `scripts/smoke` checks the change against the real ones:
it runs this checkout's `bin/flow` in every `~/Work` repo with a `.flow.json`
(or the repos given as arguments; `--flow PATH` tries another copy), runs
`flow status` and the session-start hook, and feeds the pre-tool-use hook a
few tool calls whose verdict follows from that repo's config and state. It
writes no files, so it is safe mid-feature (in ship, `flow status` does
`git fetch` the deploy branch). A `hook error` or a wrong verdict fails it. It
is `tests/smoke.rs`'s ignored `real_repos` test, run by cargo in release mode
(so the launcher finds `target/release/flow` fresh); the script passes
`--flow` and the repos on as `FLOW_SMOKE_BIN` and `FLOW_SMOKE_REPOS`
(absolute, whitespace-separated).

## Releasing

A release is what bin/flow runs for everyone without a build of their own, so
`VERSION` and `SHA256SUMS` change together, after the release exists:

1. On a branch, set `VERSION` and the `version` in `Cargo.toml` to the new
   version, commit, and push the tag `v<VERSION>` for that commit (not main).
   `.github/workflows/release.yml` builds the four binaries and publishes them
   with `SHA256SUMS`, whose `source` line is the hash of the tagged sources.
   A published release is never changed; a fix needs a new version.
2. `scripts/pin-release` copies that `SHA256SUMS` into the repo, after
   checking it lists all four binaries and this checkout's sources. Commit it,
   and merge the branch into main.

On main, CI checks that `VERSION`, `Cargo.toml` and `SHA256SUMS` agree with
the published release, and warns when `src/`, `templates/` or `Cargo.*` have
changed since (users without cargo then run the older release). Until the
first release is pinned, bin/flow builds flow from source everywhere.
