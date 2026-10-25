# Installing

## Requirements

git and a POSIX sh; `gh` for the `github` deploy provider. On Linux or macOS
(x86_64 or aarch64) no Rust is needed: bin/flow downloads the release binary
on first use (with `curl` or `wget`, checked with `sha256sum` or `shasum`). On
other Unix systems, and for a checkout whose sources have no release, it
builds flow with cargo (Rust 1.85 or newer). Windows is not supported. See
[The flow binary](#the-flow-binary).

## The plugin

The repo (GitHub `evedes/zyker-flow`) is both the plugin and a one-plugin
marketplace (`zyker-flow`). Install it once and it serves every repo; the
hooks do nothing in repos without a `.flow.json`.

From a local checkout:

```sh
claude plugin marketplace add ~/zyker-flow
claude plugin install flow@zyker-flow
```

A marketplace added from a local directory loads the plugin in place
(`claude plugin list --json` shows `readFromFolder`): commits there, and
uncommitted edits, reach every session started afterwards, or after
`/reload-plugins`. Claude Code still keeps a copy per commit under
`plugins/cache/zyker-flow/flow/<sha>/` for its records; the hooks don't run
from it.

From a git URL instead
(`claude plugin marketplace add https://github.com/evedes/zyker-flow.git`),
it is copied into the plugin cache at a commit; pick up new commits with
`claude plugin marketplace update zyker-flow` and
`claude plugin update flow@zyker-flow`.

A commit with a new `VERSION` fetches that release on its first hook call;
older ones stay in the cache (`~/.cache/flow/<VERSION>/`) until deleted. To
try a working copy for one session without installing:
`claude --plugin-dir ~/zyker-flow`.

## Per machine and per repo

The plugin brings the hooks, the skill, and `bin/` on the Bash tool's PATH.
Two things stay per machine or per repo:

### `flow` for the user

`!` commands and terminals aren't documented to get the plugin's PATH, so put
a wrapper on yours:

```sh
#!/bin/sh
# ~/.local/bin/flow
exec "$HOME/zyker-flow/bin/flow" "$@"
```

### Permission deny rules

A plugin's settings can't set permissions, so `flow init` writes them into the
repo's `.claude/settings.json`: `Bash(flow approve:*)` and the other user-only
commands, plus `Edit(<work_dir>/flow.json)` and `Edit(<work_dir>/*/state.json)`
(an `Edit` rule covers Write and NotebookEdit too; Claude Code ignores
`Write(path)` rules). The hook enforces the same things without them; they are
a second layer.

Don't also wire the hooks in a repo's own `.claude/settings.json`: plugin and
project hooks both run, so every check would run twice.

## Opting a repo in

Run `flow init` at its root, then edit `.flow.json` and `flow/STANDING.md`,
and, when the repo already has code, have its living spec written before the
first feature (see [The living spec](living-spec.md)). The hooks apply to
whatever a session does in the repo, wherever the session was started; start
it at (or under) that root so the session-start hook reports the active
feature.

## The flow binary

`hooks/hooks.json` runs `bin/flow`, a POSIX sh launcher, on every tool call.
It runs the first of:

1. `$FLOW_BIN`, when set (an absolute path).
2. `target/release/flow` in the plugin, a build of this checkout. When a file
   under `src/` or `templates/`, `Cargo.toml` or `Cargo.lock` is newer, it is
   rebuilt first (`cargo build --release --locked`, its output on stderr; if it
   fails, the previous build runs, with a warning); a source saved while that
   build runs is built next time. A hook call outside a flow repo runs the
   previous build without rebuilding, since it prints nothing there.
3. The release binary of `VERSION` for this platform, cached in
   `${FLOW_CACHE_DIR:-${XDG_CACHE_HOME:-~/.cache}/flow}/<VERSION>/<target>/flow`
   and downloaded from the `v<VERSION>` GitHub release when missing. It must
   match the `SHA256SUMS` committed next to `VERSION` (so the plugin's commit
   pins the binary, not the release page), it must run here, and it must have
   been built from this checkout's sources (the `source` line of
   `SHA256SUMS`, as `scripts/source-hash` computes it; a `SHA256SUMS` without
   that line counts as other sources). Once verified, a stamp in `target/`
   naming that binary lets later calls skip the checks until the sources,
   `VERSION` or `SHA256SUMS` change.
4. Otherwise flow is built from source with cargo, into `target/` (step 2
   from then on), without downloading the release first. A checkout with
   unreleased changes to `src/` therefore builds itself; without cargo (or if
   the build fails) it runs the release built from other sources, with a
   warning on stderr.

With none of these the hooks fail closed: in a repo with a `.flow.json`,
pre-tool-use denies every call (`no flow binary: <why>`) and session-start
says why; elsewhere they do nothing, also offline. A binary that cannot be
executed (another platform's, a `noexec` mount, a corrupt file) is caught too:
in hook mode the launcher runs the binary as a child and denies when the
shell reports 126 or 127, and drops such a cached file. The pre-tool-use hook
also has `"onFailure": "block"`, so a launcher that fails or times out blocks
the call instead of letting it through.

A download is tried once (60 s at most) and a failure is remembered for five
minutes, so an offline machine is not held up on every call. `FLOW_BIN`,
`FLOW_CACHE_DIR`, `XDG_CACHE_HOME` and `HOME` are ignored unless they are
absolute, since a relative one would resolve inside the guarded repo.

The launcher exports `FLOW_SELF`, its own path with symlinks resolved, to the
binary it runs. flow uses it where it names itself (`flow` when the `flow` on
PATH is this launcher or a short script naming it, else this path, relative to
the git root when inside it); a binary run directly names its own path.
