//! Checkpoints: the working tree as it was on entering a phase, kept as a commit under
//! refs/flow/<id>/<phase>, outside every branch (and never pushed); and `flow rewind`,
//! which puts the feature's folder and the protected code back to one of them.

use crate::config::{Event, Phase};
use crate::deploy::exit_code;
use crate::error::{bail, Result};
use crate::paths::{flow_cmd, io_error};
use crate::repo::{git, git_with, Repo};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The phases a feature can be rewound to: ship's code may be committed, and a commit
/// is not undone by putting files back.
pub const REWINDABLE: [Phase; 5] =
    [Phase::Explore, Phase::Spec, Phase::Tickets, Phase::Build, Phase::Review];

fn ref_name(fid: &str, name: &str) -> String {
    format!("refs/flow/{fid}/{name}")
}

/// A file in the git dir, for flow's own scratch use.
fn git_path(repo: &Repo, name: &str) -> Result<PathBuf> {
    let path = git(&repo.root, &["rev-parse", "--git-path", name])?;
    Ok(repo.root.join(path.trim()))
}

/// The commit `name` points at, if it exists.
fn resolve(repo: &Repo, name: &str) -> Option<String> {
    let spec = format!("{name}^{{commit}}");
    git(&repo.root, &["rev-parse", "--verify", "-q", &spec]).ok().map(|s| s.trim().to_string())
}

/// Saves every file git sees (staged, changed or untracked, not ignored) as the commit
/// refs/flow/<fid>/<name>, its parent HEAD. The real index is left alone.
pub fn save(repo: &Repo, fid: &str, name: &str) -> Result<String> {
    let index = git_path(repo, "flow-index")?;
    let real = git_path(repo, "index")?;
    if real.is_file() {
        fs::copy(&real, &index).map_err(|e| io_error(&index, e))?;
    }
    let env = [("GIT_INDEX_FILE", index.as_os_str())];
    let tree = git_with(&repo.root, &env, &["add", "-A"])
        .and_then(|_| git_with(&repo.root, &env, &["write-tree"]));
    let _ = fs::remove_file(&index);
    let tree = tree?;
    let message = format!("flow: {fid} {name}");
    // flow's own identity: the commit is internal, and must not fail for want of one.
    let mut args = vec!["-c", "user.name=flow", "-c", "user.email=flow@localhost"];
    args.extend(["commit-tree", tree.trim(), "-m", &message]);
    let head = resolve(repo, "HEAD");
    if let Some(head) = &head {
        args.extend(["-p", head]);
    }
    let commit = git(&repo.root, &args)?.trim().to_string();
    git(&repo.root, &["update-ref", &ref_name(fid, name), &commit])?;
    Ok(commit)
}

/// The checkpoint on entering `phase`, saved by `flow new` and `flow advance`. A repo
/// git can't snapshot (no commits to hang it on, say) still moves on: rewinding to that
/// phase is what it loses.
pub fn on_entering(repo: &Repo, fid: &str, phase: Phase) {
    if REWINDABLE.contains(&phase) {
        if let Err(e) = save(repo, fid, phase.name()) {
            eprintln!("flow: no checkpoint for {phase}, so it can't be rewound to: {e}");
        }
    }
}

/// `flow rewind [<phase>]`: without one, lists the checkpoints.
pub fn rewind(repo: &Repo, args: &[String]) -> Result<i32> {
    let (fid, mut st) = repo.require_active()?;
    let cmd = flow_cmd(&repo.root);
    let Some(target) = args.first() else {
        let refs = git(
            &repo.root,
            &[
                "for-each-ref",
                "--format=%(refname:lstrip=3)  %(creatordate:iso)",
                &ref_name(&fid, ""),
            ],
        )?;
        print!("{}", if refs.is_empty() { "no checkpoints\n" } else { &refs });
        return Ok(0);
    };
    let usage = "usage: flow rewind [explore|spec|tickets|build|review]";
    let Some(target) = REWINDABLE.into_iter().find(|p| p.name() == target) else {
        bail!("{usage}");
    };
    let at = |p: Phase| Phase::ALL.iter().position(|&q| q == p);
    if args.len() > 1 || at(target) > at(st.phase) {
        bail!("{fid} is in {}; it can be rewound to that phase or an earlier one", st.phase);
    }
    let Some(snap) = resolve(repo, &ref_name(&fid, target.name())) else {
        bail!("{fid} has no checkpoint for {target} (it was entered before flow kept them)");
    };
    let mut paths: Vec<String> = repo.protected.clone();
    let folder = format!("{}/{fid}", repo.work_rel);
    paths.push(folder.clone());
    let pathspec = |args: &[&str]| -> Vec<String> {
        let mut all: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        all.push("--".into());
        all.extend(paths.iter().cloned());
        all
    };
    let literal = [("GIT_LITERAL_PATHSPECS", OsStr::new("1"))];
    let run = |args: Vec<String>| {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git_with(&repo.root, &literal, &args)
    };
    // Code committed since the checkpoint is not undone by putting files back.
    if let (Some(base), Some(head)) = (resolve(repo, &format!("{snap}^")), resolve(repo, "HEAD")) {
        let mut diff = vec!["diff".to_string(), "--name-only".into(), base, head, "--".into()];
        diff.extend(repo.protected.iter().cloned());
        let committed = run(diff)?;
        if !committed.trim().is_empty() {
            bail!(
                "code was committed since {fid} entered {target} ({}); rewind puts files back \
                 but can't undo a commit",
                committed.lines().take(3).collect::<Vec<_>>().join(", ")
            );
        }
    }

    // The work it discards is kept first.
    let attempts = git(&repo.root, &["for-each-ref", "--format=%(refname)", &ref_name(&fid, "")])?;
    let n = (1..).find(|n| !attempts.lines().any(|r| r == ref_name(&fid, &format!("attempt-{n}"))));
    let attempt = format!("attempt-{}", n.unwrap_or(1));
    save(repo, &fid, &attempt)?;

    let state = format!("{folder}/state.json");
    let listed = |out: String| -> BTreeSet<String> {
        out.split('\0').filter(|f| !f.is_empty() && *f != state).map(String::from).collect()
    };
    let then = listed(run(pathspec(&["ls-tree", "-r", "-z", "--name-only", &snap]))?);
    let now = listed(run(pathspec(&["ls-files", "-z", "-co", "--exclude-standard"]))?);
    run(pathspec(&["reset", "-q"]))?;
    for gone in now.difference(&then) {
        let path = repo.root.join(gone);
        fs::remove_file(&path).map_err(|e| io_error(&path, e))?;
    }
    if !then.is_empty() {
        let list = git_path(repo, "flow-rewind")?;
        let names: Vec<&str> = then.iter().map(String::as_str).collect();
        fs::write(&list, names.join("\0")).map_err(|e| io_error(&list, e))?;
        let from = format!("--pathspec-from-file={}", list.display());
        let source = format!("--source={snap}");
        let restored = git_with(
            &repo.root,
            &literal,
            &["restore", &source, "--worktree", &from, "--pathspec-file-nul"],
        );
        let _ = fs::remove_file(&list);
        restored?;
    }

    // What was given after the checkpoint goes with the work it was given on.
    let from = at(target);
    if let Some(approvals) = st.approvals.as_mut() {
        approvals.retain(|kind, _| Phase::ALL.iter().position(|p| p.name() == kind) < from);
    }
    if at(target) <= at(Phase::Build) {
        st.gates = Some(None);
    }
    st.reviewed_fingerprint = None;
    st.phase = target;
    st.history.push(Event { rewound: Some(attempt.clone()), ..Event::now(target) });
    repo.save_state(&fid, &st)?;
    let kept = ref_name(&fid, &attempt);
    println!(
        "{fid}: rewound to {target}; its folder and the protected code are as they were on \
         entering it. The work this discarded is kept as {kept}: `git diff {kept}` shows it, \
         `git restore --source={kept} -- <path>` takes a file back. Continue with `{cmd} status`."
    );
    if !run_resets(repo)? {
        eprintln!(
            "flow: rewound, but a reset failed: the environment may still hold what the \
             discarded work left there. Fix it and run `{cmd} reset`."
        );
        return Ok(1);
    }
    Ok(0)
}

/// `flow reset`: runs .flow.json's reset commands.
pub fn reset(repo: &Repo) -> Result<i32> {
    if repo.config.reset.is_empty() {
        println!("no reset commands in .flow.json");
        return Ok(0);
    }
    Ok(if run_resets(repo)? { 0 } else { 1 })
}

/// Runs the reset commands at the git root, in order, until one fails: whether all passed.
fn run_resets(repo: &Repo) -> Result<bool> {
    for command in &repo.config.reset {
        println!("\n== [reset] {command}");
        let mut sh = Command::new("/bin/sh");
        sh.arg("-c").arg(command).current_dir(&repo.root).stdin(Stdio::null());
        let status = sh.status().map_err(|e| crate::error::Error(format!("/bin/sh: {e}")))?;
        let exit = exit_code(status);
        if exit != 0 {
            println!("== FAILED (exit {exit})");
            return Ok(false);
        }
    }
    Ok(true)
}
