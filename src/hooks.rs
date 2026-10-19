//! The Claude Code hooks: PreToolUse decides, SessionStart reports.

use crate::checks::check;
use crate::config::{given, Phase};
use crate::deploy::{self, run_list};
use crate::error::{bail, Error, Result};
use crate::paths::flow_cmd;
use crate::repo::{find_root, Repo};
use crate::shell::{
    absolute_words, bash_commands, commits_index_only, redirect_targets, without_discards,
    GIT_COMMIT, GIT_PUSH, MUTATES, USER_ONLY, USER_ONLY_CMD,
};
use serde_json::{json, Value};
use std::io;
use std::path::PathBuf;

const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];
const STATE_FILES: &str = "state files are written only by the flow CLI";

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

/// The repo the payload's cwd (else ours) is in, when it uses flow.
pub fn open_repo(payload: &Value) -> Result<Option<Repo>> {
    let cwd = match payload.get("cwd").filter(given) {
        Some(cwd) => PathBuf::from(text(Some(cwd), "cwd")?),
        None => std::env::current_dir()?,
    };
    find_root(&cwd)?.map(|root| Repo::open(&root)).transpose()
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

/// The reason to deny the tool call, or None to allow it.
fn pre_tool_use(payload: Value) -> Result<Option<String>> {
    let Some(repo) = open_repo(&payload)? else {
        return Ok(None);
    };
    let tool = payload.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let input = payload.get("tool_input").filter(given);
    let field = |key: &str| match input {
        Some(Value::Object(m)) => Ok(m.get(key)),
        Some(v) => bail!("tool_input is not an object: {v}"),
        None => Ok(None),
    };
    let paused = repo.load_global()?.paused();

    if EDIT_TOOLS.contains(&tool) {
        let path = match field("file_path")?.filter(given) {
            Some(p) => Some(p),
            None => field("notebook_path")?.filter(given),
        };
        let Some(rel) = repo.rel(text(path, "file_path")?) else {
            return Ok(None);
        };
        if repo.is_state_file(&rel) {
            return Ok(Some(STATE_FILES.into()));
        }
        if !paused && repo.is_protected(&rel) {
            return code_edit_problem(&repo);
        }
        return Ok(None);
    }
    if tool != "Bash" {
        return Ok(None);
    }

    let cmds = bash_commands(text(field("command")?, "command")?);
    if cmds.iter().any(|c| USER_ONLY_CMD.is_match(c)) {
        let (names, cmd) = (USER_ONLY.join("/"), flow_cmd(&repo.root));
        return Ok(Some(format!("{names} are the user's: they run them as `! {cmd} ...`")));
    }
    // What a mutating command must not name: a state file, a protected path.
    let work = regex::escape(&repo.work_rel);
    let names_state = repo.names(&format!(r#"{work}/(?:flow\.json|[^/\s'"]+/state\.json)"#))?;
    let protected: Vec<String> = repo.protected.iter().map(|p| regex::escape(p)).collect();
    let names_code = (!protected.is_empty()).then(|| repo.names(&protected.join("|")));
    let names_code = names_code.transpose()?;
    let (mut writes_state, mut writes_code, mut codegen) = (false, false, false);
    for c in &cmds {
        let c = without_discards(c);
        let targets: Vec<String> =
            redirect_targets(&c).iter().filter_map(|t| repo.rel(t)).collect();
        writes_state |= targets.iter().any(|t| repo.is_state_file(t));
        writes_code |= targets.iter().any(|t| repo.is_protected(t));
        if MUTATES.is_match(&c) {
            // An absolute path names what it resolves to, symlinks and all.
            let abs: Vec<String> = absolute_words(&c).iter().filter_map(|w| repo.rel(w)).collect();
            writes_state |= names_state.is_match(&c) || abs.iter().any(|r| repo.is_state_file(r));
            let code = names_code.as_ref().is_some_and(|re| re.is_match(&c));
            writes_code |= code || abs.iter().any(|r| repo.is_protected(r));
        }
        codegen = codegen || any_match(&repo.codegen, &c)?;
    }
    if writes_state {
        return Ok(Some(STATE_FILES.into()));
    }
    if paused {
        return Ok(None);
    }
    if cmds.iter().any(|c| GIT_COMMIT.is_match(c)) {
        if let Some(problem) = commit_problem(&repo, commits_index_only(&cmds))? {
            return Ok(Some(problem));
        }
    }
    if cmds.iter().any(|c| GIT_PUSH.is_match(c)) {
        if let Some(problem) = push_problem(&repo) {
            return Ok(Some(problem));
        }
    }
    if codegen || writes_code {
        return code_edit_problem(&repo);
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
    let mut ctx = match active {
        Err(broken) => broken,
        Ok(Some((fid, st))) => {
            let mut ctx =
                format!("flow: active feature {fid} ({}) is in phase {}.", st.title, st.phase);
            let problems =
                if st.phase == Phase::Ship { Vec::new() } else { check(&repo, &fid, &st, true)? };
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
    let out =
        json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": ctx}});
    Ok(Some(out.to_string()))
}
