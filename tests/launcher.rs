//! End-to-end tests of bin/flow, the sh launcher: each way it finds a binary (FLOW_BIN,
//! the dev build, the pinned release in the cache, a download, a build from source)
//! and failing closed. Each test runs a copy of the launcher in a throwaway plugin root,
//! with stand-in sh scripts for the flow binary and for curl, wget, cargo and uname, so
//! nothing touches the network or a real build. $FLOW_TEST_SH (an absolute path, e.g. to
//! dash or zsh) runs the launcher under that shell instead of /bin/sh.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

const VERSION: &str = "0.1.0";
const TRIPLE: &str = "x86_64-unknown-linux-musl";
const BINARY: &str = "flow-x86_64-unknown-linux-musl";
const RELEASE: &str = "https://github.com/evedes/zyker-flow/releases/download/v0.1.0";
const PAYLOAD: &str = r#"{"tool_name": "Bash", "tool_input": {"command": "ls"}}"#;

/// The real tools on the launcher's PATH, besides the stand-ins.
const SYSTEM_TOOLS: &str = "sh cat cp mkdir mktemp mkfifo mv chmod rm touch readlink git \
                            sha256sum shasum date find sort tr sed cut dirname";

/// Serves $FAKE_SERVER/<file>. With $FAKE_GATE (a FIFO), the first of two downloads
/// waits for the second, so that both are in flight at once.
const FAKE_CURL: &str = r#"#!/bin/sh
out= url=
while [ $# -gt 0 ]; do
  case $1 in -o) out=$2; shift ;; -*) ;; *) url=$1 ;; esac
  shift
done
echo "curl $url" >>"$FAKE_LOG"
echo "curl noise on stdout"
if [ -n "${FAKE_GATE:-}" ]; then
  if mkdir "$FAKE_GATE.first" 2>/dev/null; then echo >"$FAKE_GATE"; else cat "$FAKE_GATE" >/dev/null; fi
fi
f=$FAKE_SERVER/${url##*/}
[ -f "$f" ] || { echo "curl: (22) 404" >&2; exit 22; }
cp "$f" "$out"
"#;

const FAKE_WGET: &str = r#"#!/bin/sh
out= url=
while [ $# -gt 0 ]; do
  case $1 in -O) out=$2; shift ;; -T|-t) shift ;; -*) ;; *) url=$1 ;; esac
  shift
done
echo "wget $url" >>"$FAKE_LOG"
f=$FAKE_SERVER/${url##*/}
[ -f "$f" ] || exit 8
cp "$f" "$out"
"#;

/// Builds the stand-in "built". $FAKE_CARGO_EDIT is a source saved during the build:
/// the launcher's build-start marker is backdated first, as if the build took a while.
const FAKE_CARGO: &str = r#"#!/bin/sh
echo "cargo $*" >>"$FAKE_LOG"
echo "cargo noise on stdout"
[ -n "${FAKE_CARGO_FAIL:-}" ] && { echo "error: could not compile" >&2; exit 101; }
[ -n "${FAKE_CARGO_NOOP:-}" ] && exit 0
while [ $# -gt 0 ]; do [ "$1" = --target-dir ] && dir=$2; shift; done
mkdir -p "$dir/release"
bin=$dir/release/flow${FAKE_CARGO_EXE:+.exe}
printf '#!/bin/sh\necho "BIN built $*"\n' >"$bin"
chmod +x "$bin"
if [ -n "${FAKE_CARGO_EDIT:-}" ]; then
  touch -t 202001010000 "$dir"/.flow-build.*
  echo edited >>"$FAKE_CARGO_EDIT"
fi
exit 0
"#;

const FAKE_UNAME: &str = "#!/bin/sh\necho \"${FAKE_UNAME:-Linux x86_64}\"\n";

/// A file the shell cannot execute (126 or 127, depending on the shell).
const UNRUNNABLE: &str = "#!/nonexistent/interpreter\n";

/// A stand-in flow binary: prints its label and args, FLOW_SELF and its stdin.
fn stand_in(label: &str) -> String {
    format!(
        "#!/bin/sh\n[ -n \"${{FAKE_RAN:-}}\" ] && touch \"$FAKE_RAN\"\n\
         echo \"BIN {label} $*\"\necho \"self=$FLOW_SELF\"\necho \"stdin=$(cat)\"\n"
    )
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn deny(why: &str) -> String {
    format!(
        "{{\"hookSpecificOutput\": {{\"hookEventName\": \"PreToolUse\", \"permissionDecision\": \
         \"deny\", \"permissionDecisionReason\": \"flow: hook error: no flow binary: {why}\"}}}}\n"
    )
}

fn repo_file(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn old() -> SystemTime {
    SystemTime::now() - Duration::from_secs(3600)
}

/// Held while this process has an executable open for writing, and while it forks: a
/// child forked meanwhile inherits that fd until it execs, and running the file then
/// fails with ETXTBSY ("Text file busy").
fn fork_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn write_exe(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let _lock = fork_lock();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Sets a file's mtime through a read-only fd, which needs no fork lock.
fn set_mtime(path: &Path, t: SystemTime) {
    fs::File::open(path).unwrap().set_modified(t).unwrap();
}

fn spawn(c: &mut Command) -> Child {
    let _lock = fork_lock();
    c.spawn().unwrap()
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

impl From<Output> for Run {
    fn from(o: Output) -> Run {
        Run {
            code: o.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&o.stdout).into_owned(),
            err: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }
}

impl Run {
    /// Ran the stand-in binary `label` with `args`.
    fn ran(&self, label: &str, args: &str) -> bool {
        self.out.lines().next() == Some(&format!("BIN {label} {args}"))
    }

    fn assert_ran(&self, label: &str, args: &str) {
        assert!(self.ran(label, args), "{}\n{}", self.out, self.err);
    }

    /// Denied because `bin` cannot be executed.
    fn assert_unrunnable(&self, bin: &str) {
        let want = deny(&format!("{bin} cannot be executed (exit 12"));
        let (prefix, _) = want.split_at(want.find("(exit 12").unwrap() + 7);
        assert!(self.out.starts_with(prefix), "{}", self.out);
        assert!(self.out.ends_with("6)\"}}\n") || self.out.ends_with("7)\"}}\n"), "{}", self.out);
        assert_eq!(self.code, 0);
    }
}

/// A throwaway directory: a plugin root with bin/flow and VERSION, a home, a cache, a
/// release server, a PATH of stand-ins and real tools, and work/ for the repos.
struct Env {
    dir: PathBuf,
    vars: Vec<(String, String)>,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Env {
    fn new() -> Env {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flow-launcher-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for d in ["home", "cache", "server", "sys", "fake", "work"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        let dir = fs::canonicalize(dir).unwrap();
        let launcher = fs::read_to_string(repo_file("bin/flow")).unwrap();
        write_exe(&dir.join("plugin/bin/flow"), &launcher);
        fs::write(dir.join("plugin/VERSION"), format!("{VERSION}\n")).unwrap();
        let path = std::env::var_os("PATH").unwrap_or_default();
        for tool in SYSTEM_TOOLS.split_whitespace() {
            if let Some(p) =
                std::env::split_paths(&path).map(|d| d.join(tool)).find(|p| p.is_file())
            {
                symlink(p, dir.join("sys").join(tool)).unwrap();
            }
        }
        write_exe(&dir.join("fake/curl"), FAKE_CURL);
        write_exe(&dir.join("fake/uname"), FAKE_UNAME);
        fs::write(dir.join("log"), "").unwrap();
        let p = |s: &str| dir.join(s).display().to_string();
        let vars = [
            ("PATH", format!("{}:{}", p("fake"), p("sys"))),
            ("HOME", p("home")),
            ("FLOW_CACHE_DIR", p("cache")),
            ("FAKE_LOG", p("log")),
            ("FAKE_SERVER", p("server")),
            ("GIT_CEILING_DIRECTORIES", dir.display().to_string()),
        ];
        let vars = vars.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        Env { dir, vars }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.join(p)
    }

    fn set(&mut self, k: &str, v: impl AsRef<Path>) {
        self.unset(k);
        let v = v.as_ref().to_str().unwrap().to_string();
        self.vars.push((k.to_string(), v));
    }

    fn unset(&mut self, k: &str) {
        self.vars.retain(|(n, _)| n != k);
    }

    fn launcher(&self) -> PathBuf {
        self.path("plugin/bin/flow")
    }

    fn cached(&self) -> PathBuf {
        self.path(&format!("cache/{VERSION}/{TRIPLE}/flow"))
    }

    fn stamp(&self) -> PathBuf {
        self.path("plugin/target/.flow-release")
    }

    fn dev(&self) -> PathBuf {
        self.path("plugin/target/release/flow")
    }

    fn log(&self) -> String {
        fs::read_to_string(self.path("log")).unwrap()
    }

    fn fake(&self, tool: &str, body: &str) {
        write_exe(&self.path(&format!("fake/{tool}")), body);
    }

    /// A git repo under work/ (with a sub/ directory), opted into flow when `flow`.
    fn repo(&self, name: &str, flow: bool) -> PathBuf {
        let r = self.path(&format!("work/{name}"));
        fs::create_dir_all(r.join("sub")).unwrap();
        let ok = spawn(Command::new("git").args(["init", "-q"]).current_dir(&r))
            .wait()
            .unwrap()
            .success();
        assert!(ok, "git init failed");
        if flow {
            fs::write(r.join(".flow.json"), "{}\n").unwrap();
        }
        r
    }

    /// Writes the plugin's committed SHA256SUMS: (name, hash) pairs.
    fn pin(&self, entries: &[(&str, &str)]) {
        let text: String = entries.iter().map(|(n, h)| format!("{h}  {n}\n")).collect();
        fs::write(self.path("plugin/SHA256SUMS"), text).unwrap();
    }

    /// Publishes `body` as this platform's release binary and pins it: as `sum` when
    /// given, else its own hash, with `source` as the sources' hash when given. Returns
    /// the binary's hash.
    fn publish(&self, body: &str, sum: Option<&str>, source: Option<&str>) -> String {
        fs::write(self.path(&format!("server/{BINARY}")), body).unwrap();
        let real = sha256_hex(body.as_bytes());
        let other = "0".repeat(64);
        let mut pins =
            vec![("flow-aarch64-apple-darwin", other.as_str()), (BINARY, sum.unwrap_or(&real))];
        pins.extend(source.map(|s| ("source", s)));
        self.pin(&pins);
        real
    }

    /// Puts `body` in the cache at `path` (relative) and pins it.
    fn cache_at(&self, path: &str, body: &str) {
        write_exe(&self.path(path), body);
        self.pin(&[(BINARY, &sha256_hex(body.as_bytes()))]);
    }

    fn cache_release(&self, label: &str) {
        self.cache_at(&format!("cache/{VERSION}/{TRIPLE}/flow"), &stand_in(label));
    }

    /// Marks the cached binary verified, as an earlier call would have.
    fn stamp_cache(&self) {
        fs::create_dir_all(self.stamp().parent().unwrap()).unwrap();
        fs::write(self.stamp(), format!("{}\n", self.cached().display())).unwrap();
        set_mtime(&self.cached(), SystemTime::now() - Duration::from_secs(60));
    }

    /// Makes the plugin a checkout: src/ (with an empty directory), templates/,
    /// Cargo.toml and Cargo.lock, all an hour old. Returns the hash of its sources,
    /// computed here independently.
    fn sources(&self) -> String {
        fs::create_dir_all(self.path("plugin/src/empty")).unwrap();
        let files =
            ["Cargo.lock", "Cargo.toml", "src/hooks/mod.rs", "src/main.rs", "templates/spec.md"];
        for f in files {
            let p = self.path("plugin").join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("{f}\n")).unwrap();
            set_mtime(&p, old());
        }
        let listing: String = files
            .iter()
            .map(|f| format!("{}  {f}\n", sha256_hex(format!("{f}\n").as_bytes())))
            .collect();
        sha256_hex(listing.as_bytes())
    }

    /// A checkout with a dev build newer than its sources, and cargo.
    fn dev_checkout(&self) {
        self.sources();
        write_exe(&self.dev(), &stand_in("dev"));
        set_mtime(&self.dev(), old() + Duration::from_secs(60));
        self.fake("cargo", FAKE_CARGO);
    }

    /// The launcher, under `sh` when given (else $FLOW_TEST_SH, else its #! line).
    fn command(&self, sh: Option<&str>, cwd: &Path, args: &[&str]) -> Command {
        let test_sh = std::env::var("FLOW_TEST_SH").ok().filter(|s| !s.is_empty());
        let mut c = match sh.map(str::to_string).or(test_sh) {
            Some(sh) => {
                let mut c = Command::new(sh);
                c.arg(self.launcher());
                c
            }
            None => Command::new(self.launcher()),
        };
        c.args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(self.vars.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn spawn(&self, cwd: &Path, args: &[&str], stdin: &str) -> Child {
        let mut child = spawn(&mut self.command(None, cwd, args));
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(stdin.as_bytes()).unwrap();
        child
    }

    fn run_in(&self, cwd: &Path, args: &[&str], stdin: &str) -> Run {
        self.spawn(cwd, args, stdin).wait_with_output().unwrap().into()
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_in(&self.path("work"), args, "")
    }

    fn hook(&self, cwd: &Path, event: &str) -> Run {
        self.run_in(cwd, &["hook", event], PAYLOAD)
    }
}

/// Output of a command run without stdin.
fn output(c: &mut Command) -> Run {
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    spawn(c).wait_with_output().unwrap().into()
}

// --- 1. FLOW_BIN ------------------------------------------------------------------

#[test]
fn flow_bin_overrides_everything_and_gets_args_stdin_and_flow_self() {
    let mut e = Env::new();
    write_exe(&e.path("own"), &stand_in("own"));
    e.cache_release("cached");
    e.set("FLOW_BIN", e.path("own"));
    let r = e.run_in(&e.path("work"), &["status", "a b", ""], "the payload");
    r.assert_ran("own", "status a b ");
    assert!(r.out.contains(&format!("self={}\n", e.launcher().display())));
    assert!(r.out.contains("stdin=the payload\n"), "{}", r.out);
    assert_eq!(r.code, 0);
}

#[test]
fn flow_bin_that_is_not_executable_fails_closed() {
    let mut e = Env::new();
    e.cache_release("cached");
    let missing = e.path("missing");
    e.set("FLOW_BIN", &missing);
    let why = format!("FLOW_BIN={} is not an executable file", missing.display());

    let r = e.run(&["status"]);
    assert_eq!((r.code, r.out, r.err), (1, "".into(), format!("flow: no flow binary: {why}\n")));

    let r = e.hook(&e.repo("app", true), "pre-tool-use");
    assert_eq!((r.code, r.out), (0, deny(&why)));
}

#[test]
fn relative_flow_bin_is_not_run() {
    let mut e = Env::new();
    let repo = e.repo("app", true);
    write_exe(&repo.join("planted"), &stand_in("planted"));
    e.set("FLOW_BIN", "planted");
    let r = e.hook(&repo, "pre-tool-use");
    let why = "FLOW_BIN=planted is not an absolute path";
    assert_eq!((r.code, r.out), (0, deny(why)));
}

#[test]
fn flow_bin_that_cannot_be_executed_denies_instead_of_failing_open() {
    let mut e = Env::new();
    write_exe(&e.path("own"), UNRUNNABLE);
    e.set("FLOW_BIN", e.path("own"));
    let r = e.hook(&e.repo("app", true), "pre-tool-use");
    r.assert_unrunnable(&format!("FLOW_BIN={}", e.path("own").display()));
}

// --- 2. the dev build -------------------------------------------------------------

#[test]
fn fresh_dev_build_runs_without_cargo() {
    let e = Env::new();
    e.dev_checkout();
    e.cache_release("cached");
    e.run(&["status"]).assert_ran("dev", "status");
    assert_eq!(e.log(), "");
}

#[test]
fn stale_dev_build_is_rebuilt_first() {
    for stale in ["src/hooks/mod.rs", "templates/spec.md", "Cargo.toml", "Cargo.lock"] {
        let e = Env::new();
        e.dev_checkout();
        fs::write(e.path("plugin").join(stale), "y").unwrap();
        let r = e.run_in(&e.repo("app", true), &["hook", "session-start"], "{}");
        r.assert_ran("built", "hook session-start");
        assert!(!r.out.contains("noise"), "{}", r.out);
        let root = e.path("plugin");
        let root = root.display();
        let want = format!(
            "cargo build --release --locked --quiet --manifest-path {root}/Cargo.toml \
             --target-dir {root}/target\n"
        );
        assert_eq!(e.log(), want, "{stale}");
    }
}

#[test]
fn a_hook_outside_a_flow_repo_runs_a_stale_build_without_rebuilding() {
    let e = Env::new();
    e.dev_checkout();
    fs::write(e.path("plugin/src/main.rs"), "y").unwrap();
    e.hook(&e.repo("plain", false), "pre-tool-use").assert_ran("dev", "hook pre-tool-use");
    assert_eq!(e.log(), "");
    e.run(&["status"]).assert_ran("built", "status");
}

#[test]
fn a_rebuild_that_changes_nothing_is_not_repeated() {
    let mut e = Env::new();
    e.dev_checkout();
    e.set("FAKE_CARGO_NOOP", "1");
    fs::write(e.path("plugin/src/main.rs"), "y").unwrap();
    e.run(&["status"]).assert_ran("dev", "status");
    e.run(&["status"]).assert_ran("dev", "status");
    assert_eq!(e.log().lines().count(), 1, "{}", e.log());
}

#[test]
fn a_source_saved_during_the_rebuild_is_built_next_time() {
    let mut e = Env::new();
    e.dev_checkout();
    let src = e.path("plugin/src/main.rs");
    fs::write(&src, "y").unwrap();
    e.set("FAKE_CARGO_EDIT", &src);
    e.run(&["status"]).assert_ran("built", "status");
    e.unset("FAKE_CARGO_EDIT");
    e.run(&["status"]).assert_ran("built", "status");
    assert_eq!(e.log().lines().count(), 2, "{}", e.log());
    e.run(&["status"]).assert_ran("built", "status");
    assert_eq!(e.log().lines().count(), 2, "{}", e.log());
}

#[test]
fn failed_rebuild_runs_the_previous_build() {
    let mut e = Env::new();
    e.dev_checkout();
    e.set("FAKE_CARGO_FAIL", "1");
    fs::write(e.path("plugin/src/main.rs"), "y").unwrap();
    let r = e.hook(&e.repo("app", true), "pre-tool-use");
    r.assert_ran("dev", "hook pre-tool-use");
    assert!(r.out.contains(&format!("stdin={PAYLOAD}")));
    assert!(!r.out.contains("noise"));
    assert!(r.err.contains("could not compile"), "{}", r.err);
    let want = format!(
        "flow: cargo build --release failed in {}; running the previous build\n",
        e.path("plugin").display()
    );
    assert!(r.err.ends_with(&want), "{}", r.err);
}

#[test]
fn dev_build_that_cannot_be_executed_denies() {
    let e = Env::new();
    e.dev_checkout();
    write_exe(&e.dev(), UNRUNNABLE);
    let r = e.hook(&e.repo("app", true), "pre-tool-use");
    r.assert_unrunnable(&e.dev().display().to_string());
    assert!(e.dev().exists(), "a dev build is not removed");
}

#[test]
fn a_windows_build_is_found_as_flow_exe() {
    let mut e = Env::new();
    e.sources();
    e.fake("cargo", FAKE_CARGO);
    e.set("FAKE_CARGO_EXE", "1");
    e.set("FAKE_UNAME", "MINGW64_NT-10.0-26100 x86_64");
    e.run(&["status"]).assert_ran("built", "status");
    e.run(&["status"]).assert_ran("built", "status");
    assert_eq!(e.log().lines().count(), 1, "{}", e.log());
}

// --- 3. the cache -----------------------------------------------------------------

#[test]
fn cached_release_binary_is_verified_once_then_run_directly() {
    let e = Env::new();
    e.publish(&stand_in("downloaded"), None, None);
    e.cache_release("cached");
    // Outside a flow repo a hook leaves an unverified binary alone.
    let r = e.hook(&e.path("work"), "pre-tool-use");
    assert_eq!((r.code, r.out), (0, "".into()));
    assert!(!e.stamp().exists());
    let r = e.hook(&e.repo("app", true), "pre-tool-use");
    r.assert_ran("cached", "hook pre-tool-use");
    assert!(r.out.contains(&format!("stdin={PAYLOAD}")));
    assert_eq!(e.log(), "");
    assert!(e.stamp().exists());
    // From then on it is not hashed again: a swapped file older than the stamp runs.
    write_exe(&e.cached(), &stand_in("swapped"));
    set_mtime(&e.cached(), SystemTime::now() - Duration::from_secs(60));
    e.run(&["status"]).assert_ran("swapped", "status");
}

#[test]
fn a_new_pin_verifies_the_cache_again() {
    let e = Env::new();
    e.cache_release("cached");
    e.run(&["status"]).assert_ran("cached", "status");
    e.publish(&stand_in("downloaded"), None, None);
    e.run(&["status"]).assert_ran("downloaded", "status");
    assert_eq!(e.log(), format!("curl {RELEASE}/{BINARY}\n"));
}

#[test]
fn the_stamp_covers_only_the_binary_it_verified() {
    let mut e = Env::new();
    e.cache_release("cached");
    e.run(&["status"]).assert_ran("cached", "status");
    // Another cache directory holding an older, unverified binary.
    write_exe(&e.path(&format!("other/{VERSION}/{TRIPLE}/flow")), &stand_in("other"));
    set_mtime(&e.path(&format!("other/{VERSION}/{TRIPLE}/flow")), old());
    e.set("FLOW_CACHE_DIR", e.path("other"));
    let r = e.run(&["status"]);
    assert!(!r.ran("other", "status"), "{}", r.out);
    assert_eq!(e.log(), format!("curl {RELEASE}/{BINARY}\n"));
}

#[test]
fn cache_dir_defaults_to_xdg_then_home() {
    let mut e = Env::new();
    e.unset("FLOW_CACHE_DIR");
    e.cache_at(&format!("home/.cache/flow/{VERSION}/{TRIPLE}/flow"), &stand_in("home"));
    e.run(&["status"]).assert_ran("home", "status");
    e.cache_at(&format!("xdg/flow/{VERSION}/{TRIPLE}/flow"), &stand_in("xdg"));
    e.set("XDG_CACHE_HOME", e.path("xdg"));
    e.run(&["status"]).assert_ran("xdg", "status");
}

#[test]
fn relative_cache_dirs_are_ignored() {
    let mut e = Env::new();
    let repo = e.repo("app", true);
    // What a Bash call in the guarded repo could plant.
    for dir in ["c", "x/flow"] {
        e.cache_at(&format!("work/app/{dir}/{VERSION}/{TRIPLE}/flow"), &stand_in("planted"));
    }
    e.unset("FLOW_CACHE_DIR");
    e.set("XDG_CACHE_HOME", "x");
    let r = e.hook(&repo, "pre-tool-use");
    assert!(!r.out.contains("planted"), "{}", r.out);
    assert_eq!(e.log(), format!("curl {RELEASE}/{BINARY}\n"));
    e.set("FLOW_CACHE_DIR", "c");
    let r = e.hook(&repo, "pre-tool-use");
    assert!(!r.out.contains("planted"), "{}", r.out);
    e.set("HOME", "rel");
    e.unset("XDG_CACHE_HOME");
    e.unset("FLOW_CACHE_DIR");
    let r = e.hook(&repo, "pre-tool-use");
    assert_eq!(r.out, deny("no cache directory (HOME is not set or not absolute)"));
}

#[test]
fn a_cached_binary_that_cannot_be_executed_denies_and_is_dropped() {
    let e = Env::new();
    // Verified earlier, but not runnable now (another host sharing the cache, a remount).
    e.cache_at(&format!("cache/{VERSION}/{TRIPLE}/flow"), UNRUNNABLE);
    e.stamp_cache();
    let r = e.hook(&e.repo("plain", false), "pre-tool-use");
    assert_eq!((r.code, r.out), (0, "".into()));
    assert!(!e.cached().exists() && !e.stamp().exists());

    write_exe(&e.cached(), UNRUNNABLE);
    e.stamp_cache();
    let r = e.hook(&e.repo("app", true), "pre-tool-use");
    r.assert_unrunnable(&e.cached().display().to_string());
    assert!(!e.cached().exists());
}

// --- 4. download ------------------------------------------------------------------

#[test]
fn download_is_verified_installed_and_then_cached() {
    let e = Env::new();
    e.publish(&stand_in("downloaded"), None, None);
    let r = e.hook(&e.repo("app", true), "pre-tool-use");
    r.assert_ran("downloaded", "hook pre-tool-use");
    assert!(r.out.contains(&format!("stdin={PAYLOAD}")));
    assert!(!r.out.contains("noise"), "{}", r.out);
    assert_eq!(e.log(), format!("curl {RELEASE}/{BINARY}\n"));
    let mode = fs::metadata(e.cached()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    let left: Vec<_> = fs::read_dir(e.cached().parent().unwrap())
        .unwrap()
        .map(|d| d.unwrap().file_name())
        .collect();
    assert_eq!(left, ["flow"]);

    e.run(&["status"]).assert_ran("downloaded", "status");
    assert_eq!(e.log().lines().count(), 1);
}

#[test]
fn download_falls_back_to_wget() {
    let e = Env::new();
    fs::remove_file(e.path("fake/curl")).unwrap();
    e.fake("wget", FAKE_WGET);
    e.publish(&stand_in("downloaded"), None, None);
    e.run(&["status"]).assert_ran("downloaded", "status");
    assert_eq!(e.log(), format!("wget {RELEASE}/{BINARY}\n"));
}

#[test]
fn a_release_binary_other_than_the_pinned_one_is_neither_installed_nor_run() {
    let mut e = Env::new();
    e.set("FAKE_RAN", e.path("ran"));
    let pinned = "f".repeat(64);
    let got = e.publish(&stand_in("replaced"), Some(&pinned), None);
    // The release's own SHA256SUMS would vouch for it: only the pin counts.
    fs::write(e.path("server/SHA256SUMS"), format!("{got}  {BINARY}\n")).unwrap();
    let r = e.hook(&e.repo("app", true), "pre-tool-use");
    let why = format!("checksum mismatch for {BINARY} v{VERSION} (got {got}, want {pinned})");
    assert_eq!((r.code, r.out), (0, deny(&why)));
    assert!(!e.path("ran").exists());
    let left = fs::read_dir(e.cached().parent().unwrap()).unwrap().count();
    assert_eq!(left, 0, "nothing installed, no temporary file left");
}

#[test]
fn a_download_that_does_not_run_here_is_not_installed() {
    let e = Env::new();
    e.publish("#!/bin/sh\nexit 3\n", None, None);
    let r = e.run(&["status"]);
    assert_eq!(r.code, 1);
    let want = format!("\nflow: no flow binary: {BINARY} v{VERSION} does not run here (exit 3)\n");
    assert!(r.err.ends_with(&want), "{}", r.err);
    assert!(!e.cached().exists());
}

#[test]
fn nothing_is_downloaded_without_a_pin() {
    let e = Env::new();
    e.publish(&stand_in("downloaded"), None, None);
    e.pin(&[("flow-x86_64-apple-darwin", &"0".repeat(64))]);
    let r = e.run(&["status"]);
    let want = format!(
        "flow: no flow binary: no checksum for {BINARY} v{VERSION} in {}/SHA256SUMS\n",
        e.path("plugin").display()
    );
    assert_eq!((r.code, r.err), (1, want));
    fs::remove_file(e.path("plugin/SHA256SUMS")).unwrap();
    assert_eq!(e.run(&["status"]).code, 1);
    assert_eq!(e.log(), "");
}

#[test]
fn unsupported_platform_fails_closed() {
    let mut e = Env::new();
    e.set("FAKE_UNAME", "Plan9 mips");
    e.publish(&stand_in("downloaded"), None, None);
    let r = e.run(&["status"]);
    let want = "flow: no flow binary: no release binary for Plan9 mips\n";
    assert_eq!((r.code, r.err.as_str()), (1, want));
    assert_eq!(e.log(), "");
}

#[test]
fn each_platform_downloads_its_own_binary_into_its_own_cache() {
    for (uname, triple) in [
        ("Linux aarch64", "aarch64-unknown-linux-musl"),
        ("Darwin x86_64", "x86_64-apple-darwin"),
        ("Darwin arm64", "aarch64-apple-darwin"),
    ] {
        let mut e = Env::new();
        e.set("FAKE_UNAME", uname);
        let zeros = "0".repeat(64);
        let names = [TRIPLE, triple].map(|t| format!("flow-{t}"));
        e.pin(&names.each_ref().map(|n| (n.as_str(), zeros.as_str())));
        // Another host's binary, sharing the cache, is never taken for this one's.
        write_exe(&e.cached(), &stand_in("other-host"));
        let r = e.run(&["status"]);
        assert!(!r.out.contains("other-host"), "{}", r.out);
        let want = format!("curl {RELEASE}/flow-{triple}");
        assert_eq!(e.log().lines().next(), Some(want.as_str()));
    }
}

#[test]
fn concurrent_launchers_both_install_and_run_it() {
    let mut e = Env::new();
    let gate = e.path("gate");
    let made = output(Command::new("mkfifo").arg(&gate));
    assert_eq!(made.code, 0, "{}", made.err);
    e.set("FAKE_GATE", &gate);
    e.publish(&stand_in("downloaded"), None, None);
    let work = e.path("work");
    let both = [e.spawn(&work, &["status"], ""), e.spawn(&work, &["status"], "")];
    for child in both {
        Run::from(child.wait_with_output().unwrap()).assert_ran("downloaded", "status");
    }
    assert_eq!(e.log().lines().count(), 2, "{}", e.log());
    assert_eq!(fs::read_to_string(e.cached()).unwrap(), stand_in("downloaded"));
    let left = fs::read_dir(e.cached().parent().unwrap()).unwrap().count();
    assert_eq!(left, 1);
}

/// No release binary to download, and no cargo; returns why.
fn unavailable() -> (Env, String) {
    let e = Env::new();
    e.publish(&stand_in("downloaded"), None, None);
    fs::remove_file(e.path(&format!("server/{BINARY}"))).unwrap();
    let why = format!("downloading {RELEASE}/{BINARY} failed (curl exit 22)");
    (e, why)
}

#[test]
fn a_failed_download_is_not_retried_for_five_minutes() {
    let (e, why) = unavailable();
    let repo = e.repo("app", true);
    assert_eq!(e.hook(&repo, "pre-tool-use").out, deny(&why));
    let again = format!("{why} (not retried for 5 minutes)");
    assert_eq!(e.hook(&repo, "pre-tool-use").out, deny(&again));
    assert_eq!(e.log().lines().count(), 1, "{}", e.log());
    // Five minutes later.
    let failed = e.cached().parent().unwrap().join(".failed");
    fs::write(&failed, format!("1 {why}\n")).unwrap();
    e.publish(&stand_in("downloaded"), None, None);
    e.hook(&repo, "pre-tool-use").assert_ran("downloaded", "hook pre-tool-use");
    assert!(!failed.exists());
}

// --- 5. a checkout: the release only for its own sources, else built ----------------

#[test]
fn a_release_built_from_these_sources_runs() {
    let e = Env::new();
    let source = e.sources();
    e.publish(&stand_in("downloaded"), None, Some(&source));
    e.fake("cargo", FAKE_CARGO);
    e.run(&["status"]).assert_ran("downloaded", "status");
    assert!(e.stamp().exists());
    assert!(!e.dev().exists());
}

/// Every shell installed here hashes the sources as the release did, and then takes the
/// fast path (zsh splits words and expands an empty directory's glob differently).
#[test]
fn every_shell_computes_the_same_source_hash() {
    let e = Env::new();
    let source = e.sources();
    e.publish(&stand_in("downloaded"), None, Some(&source));
    let path = std::env::var_os("PATH").unwrap_or_default();
    for sh in ["sh", "bash", "dash", "zsh", "ksh", "mksh"] {
        let Some(sh) = std::env::split_paths(&path).map(|d| d.join(sh)).find(|p| p.is_file())
        else {
            continue;
        };
        let _ = fs::remove_file(e.stamp());
        for _ in 0..2 {
            let r = output(&mut e.command(sh.to_str(), &e.path("work"), &["status"]));
            assert!(r.ran("downloaded", "status"), "{}: {}{}", sh.display(), r.out, r.err);
            assert!(e.stamp().exists(), "{}", sh.display());
        }
    }
    assert_eq!(e.log().lines().count(), 1, "{}", e.log());
}

#[test]
fn a_release_from_other_sources_is_built_from_source_with_cargo() {
    let e = Env::new();
    e.sources();
    e.publish(&stand_in("downloaded"), None, Some(&"1".repeat(64)));
    e.fake("cargo", FAKE_CARGO);
    let r = e.run(&["status"]);
    r.assert_ran("built", "status");
    let want = format!(
        "flow: v{VERSION} was released from other sources than {}; building it from source\n",
        e.path("plugin").display()
    );
    assert!(r.err.contains(&want), "{}", r.err);
    assert_eq!(e.log().lines().filter(|l| l.starts_with("curl")).count(), 0);
    e.run(&["status"]).assert_ran("built", "status");
}

#[test]
fn a_release_from_other_sources_runs_with_a_warning_without_cargo() {
    let e = Env::new();
    e.sources();
    e.publish(&stand_in("downloaded"), None, Some(&"1".repeat(64)));
    let r = e.run(&["status"]);
    r.assert_ran("downloaded", "status");
    let want = format!(
        "flow: warning: v{VERSION} was released from other sources than {}; no cargo to \
         build it from source; running the release\n",
        e.path("plugin").display()
    );
    assert!(r.err.ends_with(&want), "{}", r.err);
    assert!(!e.stamp().exists(), "checked again on every call");
}

#[test]
fn an_updated_source_checks_the_release_again() {
    let e = Env::new();
    let source = e.sources();
    e.publish(&stand_in("downloaded"), None, Some(&source));
    e.run(&["status"]).assert_ran("downloaded", "status");
    fs::write(e.path("plugin/templates/spec.md"), "changed\n").unwrap();
    e.fake("cargo", FAKE_CARGO);
    e.run(&["status"]).assert_ran("built", "status");
}

#[test]
fn scripts_source_hash_agrees_with_the_launcher() {
    let e = Env::new();
    let source = e.sources();
    let script = e.path("plugin/scripts/source-hash");
    write_exe(&script, &fs::read_to_string(repo_file("scripts/source-hash")).unwrap());
    let r = output(Command::new(&script).env_clear().envs(e.vars.iter().cloned()));
    assert_eq!(r.out, format!("{source}\n"), "{}", r.err);
}

#[test]
fn failed_download_builds_from_source() {
    let e = Env::new();
    let source = e.sources();
    e.publish(&stand_in("downloaded"), None, Some(&source));
    fs::remove_file(e.path(&format!("server/{BINARY}"))).unwrap();
    e.fake("cargo", FAKE_CARGO);
    let r = e.run_in(&e.repo("app", true), &["hook", "session-start"], "{}");
    r.assert_ran("built", "hook session-start");
    assert!(!r.out.contains("noise"), "{}", r.out);
    let log = e.log();
    let want = format!("curl {RELEASE}/{BINARY}\ncargo build --release");
    assert!(log.starts_with(&want), "{log}");
    let want = format!(
        "flow: downloading {RELEASE}/{BINARY} failed (curl exit 22); building it from source\n"
    );
    assert!(r.err.contains(&want), "{}", r.err);
}

#[test]
fn no_cargo_is_not_announced_as_building() {
    let e = Env::new();
    e.sources();
    let r = e.run(&["status"]);
    assert_eq!(r.code, 1);
    assert!(!r.err.contains("building it"), "{}", r.err);
    let want = "/SHA256SUMS; no cargo to build it from source\n";
    assert!(r.err.ends_with(want), "{}", r.err);
}

// --- 6. failing closed ------------------------------------------------------------

#[test]
fn pre_tool_use_denies_in_a_flow_repo() {
    let (e, why) = unavailable();
    let r = e.hook(&e.repo("app", true).join("sub"), "pre-tool-use");
    assert_eq!((r.code, r.out), (0, deny(&why)), "{}", r.err);
}

#[test]
fn hooks_outside_a_flow_repo_stay_silent_and_offline() {
    let (e, _) = unavailable();
    for cwd in [e.repo("plain", false), e.path("work")] {
        for event in ["pre-tool-use", "session-start"] {
            let r = e.hook(&cwd, event);
            assert_eq!((r.code, r.out), (0, "".into()), "{event}");
        }
    }
    assert_eq!(e.log(), "", "no download outside a flow repo");
}

#[test]
fn session_start_says_why_in_a_flow_repo() {
    let (e, why) = unavailable();
    let r = e.run_in(&e.repo("app", true), &["hook", "session-start"], "{}");
    let ctx = format!(
        "flow: no flow binary ({why}), so the flow hook denies every Edit, Write, MultiEdit, \
         NotebookEdit and Bash call in this repo. Tell the user: the plugin's bin/flow needs \
         network access to download its release binary, cargo to build it, or FLOW_BIN set \
         to one."
    );
    let want = format!(
        "{{\"hookSpecificOutput\": {{\"hookEventName\": \"SessionStart\", \
         \"additionalContext\": \"{ctx}\"}}}}\n"
    );
    assert_eq!((r.code, r.out), (0, want));
}

#[test]
fn other_hook_events_stay_silent() {
    let (e, _) = unavailable();
    let r = e.hook(&e.repo("app", true), "post-tool-use");
    assert_eq!((r.code, r.out), (0, "".into()));
}

#[test]
fn cli_without_a_binary_exits_1() {
    let (e, why) = unavailable();
    let repo = e.repo("app", true);
    for args in [&["status"][..], &["hook"], &[]] {
        let r = e.run_in(&repo, args, "");
        assert_eq!((r.code, r.out.as_str()), (1, ""), "{args:?}");
        // The first call tries the download; later ones remember that it failed.
        let last = r.err.lines().last().unwrap_or_default();
        assert!(last.starts_with(&format!("flow: no flow binary: {why}")), "{}", r.err);
    }
}

#[test]
fn deny_reason_is_escaped_json() {
    let mut e = Env::new();
    let repo = e.repo("app", true);
    e.set("FLOW_BIN", e.path("a\"b\\c\td\ne\x01f"));
    let r = e.hook(&repo, "pre-tool-use");
    let why = format!("FLOW_BIN={}/a\\\"b\\\\c d e f is not an executable file", e.dir.display());
    assert_eq!(r.out, deny(&why));
}

// --- the plugin's hook config -------------------------------------------------------

#[test]
fn hooks_run_the_launcher_and_a_failing_pre_tool_use_hook_blocks() {
    let text = fs::read_to_string(repo_file("hooks/hooks.json")).unwrap();
    let hooks: serde_json::Value = serde_json::from_str(&text).unwrap();
    let pre = &hooks["hooks"]["PreToolUse"][0];
    assert_eq!(pre["matcher"], "Edit|Write|MultiEdit|NotebookEdit|Bash");
    let cmd = &pre["hooks"][0];
    assert_eq!(cmd["command"], "\"${CLAUDE_PLUGIN_ROOT}\"/bin/flow hook pre-tool-use");
    assert_eq!(cmd["onFailure"], "block", "a launcher that fails or times out blocks");
    let start = &hooks["hooks"]["SessionStart"][0]["hooks"][0];
    assert_eq!(start["command"], "\"${CLAUDE_PLUGIN_ROOT}\"/bin/flow hook session-start");
}

// --- locating the plugin root ------------------------------------------------------

#[test]
fn symlinked_and_relative_launchers_find_their_root() {
    let e = Env::new();
    e.cache_release("cached");
    fs::create_dir_all(e.path("home/.local/bin")).unwrap();
    symlink("../../../plugin/bin/flow", e.path("home/.local/bin/flow")).unwrap();
    symlink(e.path("home/.local/bin/flow"), e.path("home/flow2")).unwrap();
    let mut runs = vec![];
    for link in ["home/.local/bin/flow", "home/flow2"] {
        // The link itself, run by its #! line.
        let mut c = Command::new(e.path(link));
        c.arg("status").current_dir(e.path("work")).env_clear().envs(e.vars.iter().cloned());
        runs.push(output(&mut c));
    }
    let sh = std::env::var("FLOW_TEST_SH").unwrap_or_else(|_| "sh".into());
    let mut c = Command::new(e.path("sys/sh"));
    c.args(["-c", &format!("{sh} ./bin/../bin/flow status")])
        .current_dir(e.path("plugin"))
        .env_clear()
        .envs(e.vars.iter().cloned());
    runs.push(output(&mut c));
    let want = format!("self={}\n", e.launcher().display());
    for r in runs {
        r.assert_ran("cached", "status");
        assert!(r.out.contains(&want), "{}", r.out);
    }
}
