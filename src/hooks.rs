//! The Claude Code hooks: PreToolUse decides, SessionStart reports.

use crate::checks::check;
use crate::config::{given, Phase};
use crate::deploy::{self, run_list};
use crate::error::{bail, Error, Result};
use crate::paths::flow_cmd;
use crate::primer::primer;
use crate::repo::{find_root, flow_root_of, Repo};
use crate::shell::{
    absolute_words, bash_commands, cd_target, commits_index_only, git_dirs, redirect_targets,
    without_discards, GIT_COMMIT, GIT_PUSH, MUTATES, USER_ONLY, USER_ONLY_CMD,
};
use serde_json::{json, Value};
use std::io;
use std::path::{Path, PathBuf};

const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];
/// The tools that run a shell `command` (Monitor streams its output as events).
const SHELL_TOOLS: [&str; 2] = ["Bash", "Monitor"];
const STATE_FILES: &str = "state files are written only by the flow CLI";

/// Why the rules can't change now: a feature is active.
fn rules_problem(repo: &Repo) -> Result<Option<String>> {
    let Some((fid, _)) = repo.active()? else {
        return Ok(None);
    };
    let work = &repo.work_rel;
    Ok(Some(format!(
        "while {fid} is active, the rules it is judged by (.flow.json, .claude/settings.json, \
         .claude/settings.local.json, {work}/STANDING.md, {work}/templates/, {work}/specs/) \
         change only by hand: propose the change and let the user make it"
    )))
}

/// A string field of the payload: "" when not given.
fn text<'a>(field: Option<&'a Value>, name: &str) -> Result<&'a str> {
    match field {
        None => Ok(""),
        Some(Value::String(s)) => Ok(s),
        Some(v) => bail!("{name} is not a string: {v}"),
    }
}

/// The payload, which must be a JSON object.
fn object(payload: Value) -> Result<Value> {
    if !payload.is_object() {
        bail!("the hook payload is not a JSON object");
    }
    Ok(payload)
}

/// The PreToolUse decision on the hook's stdin: the reason to deny, or None to allow.
/// Claude Code always sends a JSON object. Stdin flow cannot read (not UTF-8, not JSON)
/// is an error where the hook's own cwd is in a flow repo, and is allowed elsewhere; a
/// payload of the wrong shape is an error anywhere.
pub fn pre_tool_use_input(stdin: io::Result<String>) -> Result<Option<String>> {
    let payload = match stdin {
        Ok(text) => serde_json::from_str::<Value>(&text).map_err(Error::from),
        Err(e) => Err(Error(format!("cannot read the hook input: {e}"))),
    };
    match payload {
        Ok(payload) => pre_tool_use(object(payload)?),
        Err(e) => match find_root(&std::env::current_dir()?)? {
            Some(_) => Err(e),
            None => Ok(None),
        },
    }
}

/// The SessionStart output on the hook's stdin; stdin it cannot read is an error.
pub fn session_start_input(stdin: io::Result<String>) -> Result<Option<String>> {
    session_start(object(serde_json::from_str(&stdin?)?)?)
}

/// The session's working directory: the payload's cwd, else ours.
fn session_dir(payload: &Value) -> Result<PathBuf> {
    let dir = match payload.get("cwd").filter(given) {
        Some(cwd) => PathBuf::from(text(Some(cwd), "cwd")?),
        None => std::env::current_dir()?,
    };
    if !dir.is_dir() {
        bail!("cwd {} is not a directory", dir.display());
    }
    Ok(dir)
}

/// The repo the session's working directory is in, when it uses flow.
fn open_repo(payload: &Value) -> Result<Option<Repo>> {
    flow_root_of(&session_dir(payload)?)?.map(|root| Repo::open(&root)).transpose()
}

/// A directory a command names (`~` expanded from $HOME), from `dir`; None when it
/// depends on what only the shell knows (a variable, a substitution).
fn locate(dir: &Path, word: &str) -> Option<PathBuf> {
    let word = match word.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = std::env::var("HOME").ok().filter(|h| h.starts_with('/'))?;
            format!("{home}{rest}")
        }
        _ => word.to_string(),
    };
    (!word.contains(['$', '`', '~'])).then(|| dir.join(word))
}

/// A flow repo a tool call touches, and what the call does to it.
struct Target {
    repo: Repo,
    /// What a mutating command must not name: a state file, a rule file, a protected path.
    names_state: regex::Regex,
    names_rules: regex::Regex,
    names_code: Option<regex::Regex>,
    writes_state: bool,
    writes_rules: bool,
    writes_code: bool,
    codegen: bool,
    commits: bool,
    pushes: bool,
}

impl Target {
    fn open(root: &Path) -> Result<Target> {
        let repo = Repo::open(root)?;
        let work = regex::escape(&repo.work_rel);
        let names_state = repo.names(&format!(r#"{work}/(?:flow\.json|[^/\s'"]+/state\.json)"#))?;
        let names_rules = repo.names(&format!(
            r"\.flow\.json|\.claude/settings(?:\.local)?\.json|{work}/(?:STANDING\.md|templates|specs)"
        ))?;
        let protected: Vec<String> = repo.protected.iter().map(|p| regex::escape(p)).collect();
        let names_code = (!protected.is_empty()).then(|| repo.names(&protected.join("|")));
        let names_code = names_code.transpose()?;
        Ok(Target {
            repo,
            names_state,
            names_rules,
            names_code,
            writes_state: false,
            writes_rules: false,
            writes_code: false,
            codegen: false,
            commits: false,
            pushes: false,
        })
    }

    /// Marks what writing `path` (relative to the root, or absolute) does here.
    fn writes(&mut self, path: &str) {
        let Some(rel) = self.repo.rel(path) else {
            return;
        };
        self.writes_state |= self.repo.is_state_file(&rel);
        self.writes_rules |= self.repo.is_rule_file(&rel);
        self.writes_code |= self.repo.is_protected(&rel);
    }

    /// Why what the call does here is refused, if it is.
    fn problem(&self, index_only: bool) -> Result<Option<String>> {
        let repo = &self.repo;
        if self.writes_state {
            return Ok(Some(STATE_FILES.into()));
        }
        if self.writes_rules {
            if let Some(problem) = rules_problem(repo)? {
                return Ok(Some(problem));
            }
        }
        if repo.load_global()?.paused() {
            return Ok(None);
        }
        if self.commits {
            if let Some(problem) = commit_problem(repo, index_only)? {
                return Ok(Some(problem));
            }
        }
        if self.pushes {
            if let Some(problem) = push_problem(repo) {
                return Ok(Some(problem));
            }
        }
        if self.codegen || self.writes_code {
            return code_edit_problem(repo);
        }
        Ok(None)
    }
}

/// The flow repos a tool call touches, each opened once.
#[derive(Default)]
struct Targets {
    open: Vec<Target>,
    /// Each path looked up, and the flow repo root it is in.
    roots: Vec<(PathBuf, Option<PathBuf>)>,
}

impl Targets {
    /// The flow repo `path` is in, if any.
    fn at(&mut self, path: &Path) -> Result<Option<&mut Target>> {
        let root = match self.roots.iter().find(|(p, _)| p == path) {
            Some((_, root)) => root.clone(),
            None => {
                let root = flow_root_of(path)?;
                self.roots.push((path.to_path_buf(), root.clone()));
                root
            }
        };
        let Some(root) = root else {
            return Ok(None);
        };
        let i = match self.open.iter().position(|t| t.repo.root == root) {
            Some(i) => i,
            None => {
                self.open.push(Target::open(&root)?);
                self.open.len() - 1
            }
        };
        Ok(Some(&mut self.open[i]))
    }
}

pub fn deny_output(reason: &str) -> String {
    json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": format!("flow: {reason}"),
    }})
    .to_string()
}

fn code_edit_problem(repo: &Repo) -> Result<Option<String>> {
    let cmd = flow_cmd(&repo.root);
    let none = format!(
        "no active feature; code changes go through `{cmd} new` (or the user can `! {cmd} pause`)"
    );
    Ok(match repo.active()? {
        None => Some(none),
        Some((fid, st)) if st.phase != Phase::Build => {
            Some(format!("{fid} is in phase {}; code is edited only in build", st.phase))
        }
        Some(_) => None,
    })
}

/// Why a commit is refused: judged by what is staged when `index_only`, else by the
/// whole working tree.
fn commit_problem(repo: &Repo, index_only: bool) -> Result<Option<String>> {
    let mut touched = if index_only { repo.staged_paths()? } else { repo.changed_paths()? };
    touched.retain(|p| repo.is_protected(p));
    if touched.is_empty() {
        return Ok(None);
    }
    let (fid, st) = match repo.active()? {
        Some((fid, st)) if st.phase == Phase::Ship => (fid, st),
        other => {
            let state = other.map_or("there is no active feature".into(), |(fid, st)| {
                format!("{fid} is in phase {}", st.phase)
            });
            let shown = touched[..touched.len().min(3)].join(", ");
            let why = format!(
                "commit touches code ({shown}) but {state}; code is committed only in ship"
            );
            return Ok(Some(why));
        }
    };
    for kind in Phase::APPROVABLE {
        if !repo.approval_valid(&fid, &st, kind)? {
            return Ok(Some(format!("{kind} approval missing or stale")));
        }
    }
    let gates = "code changed since the gates passed: `flow back` to build and rerun `flow gates`";
    let Some(passed) = st.gates().filter(|g| g.passed) else {
        return Ok(Some(gates.into()));
    };
    let fingerprint = repo.fingerprint()?;
    let why = if passed.fingerprint != fingerprint {
        gates
    } else if st.reviewed_fingerprint.flatten() != Some(fingerprint) {
        "code changed since review passed: go back and review again"
    } else {
        return Ok(None);
    };
    Ok(Some(why.into()))
}

fn push_problem(repo: &Repo) -> Option<String> {
    let runs = match deploy::runs(repo.config.deploy.as_ref()?, &repo.root, 30) {
        Ok(runs) => runs,
        Err(e) => return Some(format!("cannot check whether a deploy is running ({e})")),
    };
    let running: Vec<_> = runs.iter().filter(|r| r.in_flight()).collect();
    let list = run_list(running);
    let why =
        format!("a CI/deploy run is in flight ({list}); pushing now cancels it. Wait and retry");
    (!list.is_empty()).then_some(why)
}

fn any_match(patterns: &[fancy_regex::Regex], text: &str) -> Result<bool> {
    for re in patterns {
        if re.is_match(text)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The reason to deny the tool call, or None to allow it. Each path and command is judged
/// by the flow repo it acts in: an absolute path by the one it is in, a relative one by
/// the one its command runs in (the session's working directory, after any `cd` before
/// it and a git command's `-C`).
fn pre_tool_use(payload: Value) -> Result<Option<String>> {
    let cwd = session_dir(&payload)?;
    let tool = payload.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let input = payload.get("tool_input").filter(given);
    let field = |key: &str| match input {
        Some(Value::Object(m)) => Ok(m.get(key)),
        Some(v) => bail!("tool_input is not an object: {v}"),
        None => Ok(None),
    };
    let mut targets = Targets::default();

    if EDIT_TOOLS.contains(&tool) {
        let path = match field("file_path")?.filter(given) {
            Some(p) => Some(p),
            None => field("notebook_path")?.filter(given),
        };
        let file = text(path, "file_path")?;
        let at = if file.starts_with('/') { PathBuf::from(file) } else { cwd };
        let Some(Target { repo, .. }) = targets.at(&at)? else {
            return Ok(None);
        };
        let Some(rel) = repo.rel(file) else {
            return Ok(None);
        };
        if repo.is_state_file(&rel) {
            return Ok(Some(STATE_FILES.into()));
        }
        if repo.is_rule_file(&rel) {
            return rules_problem(repo);
        }
        if !repo.load_global()?.paused() && repo.is_protected(&rel) {
            return code_edit_problem(repo);
        }
        return Ok(None);
    }
    if !SHELL_TOOLS.contains(&tool) {
        return Ok(None);
    }

    let cmds = bash_commands(text(field("command")?, "command")?);
    // Where the commands run: a `cd` moves the ones after it, unless it can't be followed
    // (to a directory that isn't there, or one only the shell can work out).
    let mut dir = cwd;
    for c in &cmds {
        let mut here = dir.clone();
        for d in git_dirs(c) {
            let Some(next) = locate(&here, &d) else { break };
            here = next;
        }
        let bare = without_discards(c);
        if let Some(t) = targets.at(&here)? {
            if USER_ONLY_CMD.is_match(c) {
                let (names, cmd) = (USER_ONLY.join("/"), flow_cmd(&t.repo.root));
                return Ok(Some(format!("{names} are the user's: they run them as `! {cmd} ...`")));
            }
            t.commits |= GIT_COMMIT.is_match(c);
            t.pushes |= GIT_PUSH.is_match(c);
            t.codegen = t.codegen || any_match(&t.repo.codegen, &bare)?;
            if MUTATES.is_match(&bare) {
                t.writes_state |= t.names_state.is_match(&bare);
                t.writes_rules |= t.names_rules.is_match(&bare);
                t.writes_code |= t.names_code.as_ref().is_some_and(|re| re.is_match(&bare));
            }
        }
        // What it writes, each path in the repo it is in. An absolute path names what it
        // resolves to, symlinks and all.
        let mut written: Vec<String> =
            redirect_targets(&bare).into_iter().map(String::from).collect();
        if MUTATES.is_match(&bare) {
            written.extend(absolute_words(&bare));
        }
        for w in written {
            let at = if w.starts_with('/') { PathBuf::from(&w) } else { here.clone() };
            if let Some(t) = targets.at(&at)? {
                t.writes(&w);
            }
        }
        if let Some(Some(to)) = cd_target(c) {
            if let Some(to) = locate(&dir, &to).filter(|to| to.is_dir()) {
                dir = to;
            }
        }
    }
    // A `cd` stages nothing: `cd ../b && git commit -m x` takes only what is staged.
    let moves: Vec<String> = cmds.into_iter().filter(|c| cd_target(c).is_none()).collect();
    let index_only = commits_index_only(&moves);
    for t in &targets.open {
        if let Some(problem) = t.problem(index_only)? {
            return Ok(Some(problem));
        }
    }
    Ok(None)
}

/// The SessionStart output: where the active feature stands.
fn session_start(payload: Value) -> Result<Option<String>> {
    let Some(repo) = open_repo(&payload)? else {
        return Ok(None);
    };
    // A missing state file of the active feature is reported, not an error.
    let active = repo.active().map_err(|e| format!("flow: {e}."));
    let mut ctx = match &active {
        Err(broken) => broken.clone(),
        Ok(Some((fid, st))) => {
            let mut ctx =
                format!("flow: active feature {fid} ({}) is in phase {}.", st.title, st.phase);
            let problems =
                if st.phase == Phase::Ship { Vec::new() } else { check(&repo, fid, st, true)? };
            if !problems.is_empty() {
                ctx += &format!(" To advance: {}", problems.join("; "));
            }
            ctx
        }
        Ok(None) => {
            let cmd = flow_cmd(&repo.root);
            format!(
                "flow: no active feature. Code changes start with `{cmd} new <slug> \"<title>\"`."
            )
        }
    };
    if repo.load_global()?.paused() {
        ctx += " Enforcement is PAUSED.";
    }
    let active = active.as_ref().ok().and_then(|a| a.as_ref()).map(|(f, st)| (f.as_str(), st));
    ctx += "\n\n";
    ctx += &primer(&repo, active)?;
    let out =
        json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": ctx}});
    Ok(Some(out.to_string()))
}
