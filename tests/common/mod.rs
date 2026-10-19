//! Helpers for the end-to-end tests: throwaway git repos opted into flow, and the flow
//! binary driven as the CLI and as the hooks.
#![allow(dead_code)]

use serde_json::{json, Value};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

pub const RESEARCH: &str = "# Research: x
## Question
What to build.
## Findings
Nothing yet.
## Options
Do it.
## Open questions
- [x] Colour? Blue.
";

pub const SPEC: &str = "# Spec: x
## Goal
A greeting.
## Behaviour
Shows hello.
## Data model
None.
## UI
None.
## Acceptance criteria
Hello is shown.
## Out of scope
Goodbyes.
## Open questions
None.
";

pub const TICKET: &str = "# T01: Show hello
## Description / desired end state
Hello shown.
## Context / why
Spec goal.
## Entry points
lib/app.ex
## In scope
lib/
## Out of scope
config/
## Gate
lib/app.ex exists.
## Verification steps
```sh
test -f lib/app.ex
```
";

pub const REVIEW: &str = "# Review: x
## Verdict
pass — matches the spec.
## Findings
None.
";

/// Elixir's code generators and formatter, as zyker configures them.
pub const MIX_CODEGEN: [&str; 3] = [
    r"\bmix\s+(phx|ecto)\.gen\.\S",
    r"\bmix\s+igniter\.\S",
    r"\bmix\s+format\b(?!.*--check-formatted)",
];

/// The binary under test: $FLOW_TEST_BIN (bin/flow, say), else the one cargo built.
pub fn flow_bin() -> PathBuf {
    match std::env::var_os("FLOW_TEST_BIN") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(env!("CARGO_BIN_EXE_flow")),
    }
}

/// A path in this checkout.
pub fn checkout(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Whether the binary under test is bin/flow, which sets FLOW_SELF itself.
pub fn testing_launcher() -> bool {
    match (fs::canonicalize(flow_bin()), fs::canonicalize(checkout("bin/flow"))) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[track_caller]
pub fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("bad JSON ({e}): {text:?}"))
}

#[track_caller]
pub fn assert_in(needle: &str, haystack: &str) {
    assert!(haystack.contains(needle), "{needle:?} not in {haystack:?}");
}

/// The reason of a denied tool call; fails the test when it was allowed.
#[track_caller]
pub fn denied(verdict: Option<String>) -> String {
    verdict.expect("allowed, expected a deny")
}

/// A UTC timestamp as flow writes it: %Y-%m-%dT%H:%M:%SZ.
pub fn is_timestamp(s: &str) -> bool {
    s.len() == 20
        && s.bytes().enumerate().all(|(i, c)| match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            19 => c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

/// A directory removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flow-test-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir); // left over from a run that reused this pid
        fs::create_dir(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        TempDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Default for TempDir {
    fn default() -> Self {
        TempDir::new()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A finished process, its output decoded as text.
#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl From<Output> for Run {
    fn from(out: Output) -> Run {
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

/// Held while a test writes a file and while it forks a child: a child forked while
/// another thread has a file open for writing holds that fd until it execs, and running
/// the file meanwhile (a stub `gh`, say) fails with ETXTBSY.
fn fork_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts `c`, holding the fork lock while it forks.
pub fn spawn(c: &mut Command) -> Child {
    let _lock = fork_lock();
    c.spawn().unwrap_or_else(|e| panic!("run {c:?}: {e}"))
}

/// Runs `c` to completion with `stdin` fed in (none: /dev/null), capturing its output.
pub fn run(c: &mut Command, stdin: Option<&[u8]>) -> Run {
    c.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn(c);
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A hook that exits without reading closes the pipe; that is not our failure.
        let _ = pipe.write_all(bytes);
    }
    Run::from(child.wait_with_output().expect("wait for child"))
}

pub fn write(path: &Path, text: impl AsRef<[u8]>) {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).unwrap_or_else(|e| panic!("mkdir {}: {e}", dir.display()));
    }
    let _lock = fork_lock();
    fs::write(path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

pub fn chmod_x(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// PATH without the directories that hold a `flow` executable.
pub fn path_without_flow() -> OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::env::split_paths(&path).filter(|d| {
        let is_exe = |m: fs::Metadata| m.is_file() && m.permissions().mode() & 0o111 != 0;
        !fs::metadata(d.join("flow")).is_ok_and(is_exe)
    });
    std::env::join_paths(dirs).expect("join PATH")
}

/// `dir` prepended to $PATH.
pub fn path_with(dir: &Path) -> OsString {
    let mut path = dir.as_os_str().to_owned();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    path
}

/// A pre-tool-use run's verdict: None when allowed, else the deny reason.
#[track_caller]
pub fn verdict(r: &Run) -> Option<String> {
    assert_eq!(r.code, 0, "{}", r.stderr);
    if r.stdout.trim().is_empty() {
        return None;
    }
    let out = &parse(&r.stdout)["hookSpecificOutput"];
    assert_eq!(out["hookEventName"], "PreToolUse", "{}", r.stdout);
    assert_eq!(out["permissionDecision"], "deny", "{}", r.stdout);
    let reason = out["permissionDecisionReason"].as_str();
    Some(reason.expect("a deny reason").to_string())
}

/// A throwaway git repo opted into flow, with one commit.
pub struct Flow {
    _tmp: TempDir,
    pub root: PathBuf,
    /// Environment changes for every process the test runs: Some sets, None removes.
    env: Vec<(String, Option<OsString>)>,
}

impl Default for Flow {
    fn default() -> Self {
        Flow::new()
    }
}

impl Flow {
    pub fn new() -> Flow {
        Flow::with_env(&[])
    }

    /// A repo whose processes, git's setup included, run with `env` set.
    pub fn with_env(env: &[(&str, &str)]) -> Flow {
        let tmp = TempDir::new();
        let mut t = Flow { root: tmp.path().to_path_buf(), _tmp: tmp, env: Vec::new() };
        // FLOW_SELF is the launcher's to set; a stray one would change how flow names itself.
        t.unset_env("FLOW_SELF");
        for (k, v) in env {
            t.set_env(k, v);
        }
        t.git(&["init", "-q", "-b", "main"]);
        t.git(&["config", "user.email", "t@example.com"]);
        t.git(&["config", "user.name", "t"]);
        // Keeps a globally enabled commit signing out of the way.
        t.git(&["config", "commit.gpgsign", "false"]);
        let cfg = json!({
            "work_dir": "work",
            "protected_paths": ["lib/", "mix.exs"],
            "standing_gates": ["true"],
            "codegen": MIX_CODEGEN,
        });
        t.write(".flow.json", cfg.to_string());
        t.write("README.md", "readme\n");
        t.git(&["add", "-A"]);
        t.git(&["commit", "-qm", "init"]);
        t
    }

    pub fn set_env(&mut self, key: &str, value: impl AsRef<OsStr>) {
        self.env.retain(|(k, _)| k != key);
        self.env.push((key.to_string(), Some(value.as_ref().to_owned())));
    }

    pub fn unset_env(&mut self, key: &str) {
        self.env.retain(|(k, _)| k != key);
        self.env.push((key.to_string(), None));
    }

    /// `program`, with the test's environment, run in the repo.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut c = Command::new(program);
        c.current_dir(&self.root);
        for (k, v) in &self.env {
            match v {
                Some(v) => c.env(k, v),
                None => c.env_remove(k),
            };
        }
        c
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn write(&self, rel: &str, text: impl AsRef<[u8]>) {
        write(&self.path(rel), text);
    }

    pub fn read(&self, rel: &str) -> String {
        read(&self.path(rel))
    }

    #[track_caller]
    pub fn git(&self, args: &[&str]) -> String {
        let r = run(self.command("git").args(args), None);
        assert_eq!(r.code, 0, "git {args:?}: {}", r.stderr);
        r.stdout
    }

    /// Runs the CLI in the repo, whatever its exit code.
    pub fn flow_any(&self, args: &[&str]) -> Run {
        run(self.command(flow_bin()).args(args), None)
    }

    /// Runs the CLI in the repo; it must succeed.
    #[track_caller]
    pub fn flow(&self, args: &[&str]) -> Run {
        let r = self.flow_any(args);
        assert_eq!(r.code, 0, "flow {args:?}: {}{}", r.stdout, r.stderr);
        r
    }

    /// Runs `flow hook <event>` in `cwd` on a raw stdin payload.
    pub fn hook_in(&self, cwd: &Path, event: &str, stdin: impl AsRef<[u8]>) -> Run {
        let mut c = self.command(flow_bin());
        c.args(["hook", event]).current_dir(cwd);
        run(&mut c, Some(stdin.as_ref()))
    }

    /// Runs `flow hook <event>` in the repo on a raw stdin payload.
    pub fn hook_raw(&self, event: &str, stdin: impl AsRef<[u8]>) -> Run {
        self.hook_in(&self.root, event, stdin)
    }

    /// The deny reason the pre-tool-use hook gives for a tool call, or None when allowed.
    #[track_caller]
    pub fn hook(&self, tool: &str, tool_input: Value) -> Option<String> {
        let payload = json!({"cwd": self.root, "tool_name": tool, "tool_input": tool_input});
        verdict(&self.hook_raw("pre-tool-use", payload.to_string()))
    }

    #[track_caller]
    pub fn bash(&self, command: &str) -> Option<String> {
        self.hook("Bash", json!({ "command": command }))
    }

    #[track_caller]
    pub fn edit(&self, rel: &str) -> Option<String> {
        self.hook("Edit", json!({ "file_path": self.path(rel) }))
    }

    pub fn fdir(&self) -> PathBuf {
        self.path("work/001-hello")
    }

    pub fn fwrite(&self, rel: &str, text: &str) {
        write(&self.fdir().join(rel), text);
    }

    pub fn fread(&self, rel: &str) -> String {
        read(&self.fdir().join(rel))
    }

    pub fn to_build(&self) {
        self.flow(&["new", "hello", "Hello"]);
        self.fwrite("research.md", RESEARCH);
        self.flow(&["advance"]);
        self.fwrite("spec.md", SPEC);
        self.flow(&["approve", "spec"]);
        self.flow(&["advance"]);
        self.fwrite("tickets/T01.md", TICKET);
        self.flow(&["approve", "tickets"]);
        self.flow(&["advance"]);
    }

    pub fn to_ship(&self) {
        self.to_build();
        self.write("lib/app.ex", "hello\n");
        self.flow(&["gates"]);
        self.flow(&["advance"]);
        self.fwrite("review.md", REVIEW);
        self.flow(&["advance"]);
    }

    pub fn state(&self) -> Value {
        parse(&self.fread("state.json"))
    }

    pub fn phase(&self) -> String {
        let st = self.state();
        st["phase"].as_str().expect("phase").to_string()
    }

    /// Sets the top-level keys of `changes` (a JSON object) in .flow.json.
    pub fn configure(&self, changes: Value) {
        let mut cfg = parse(&self.read(".flow.json"));
        let (Some(cfg_map), Value::Object(changes)) = (cfg.as_object_mut(), changes) else {
            panic!(".flow.json and the changes must be objects");
        };
        cfg_map.extend(changes);
        self.write(".flow.json", cfg.to_string());
    }
}
