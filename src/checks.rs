//! What each phase needs before `flow advance` moves on.

use crate::config::{Phase, State};
use crate::deploy::{self, run_list};
use crate::error::Result;
use crate::markdown::{check_doc, gate_commands, unchecked_boxes, Doc};
use crate::paths::{file_name, flow_cmd};
use crate::repo::{git, Repo};
use regex::Regex;
use std::process::{Command, Stdio};
use std::sync::LazyLock;

/// `\w` is a letter, a number or `_`: a combining mark after "pass" ends the word.
static VERDICT_PASS: LazyLock<Regex> = regex!(r"(?i)^[^\p{L}\p{N}_]*pass(?:[^\p{L}\p{N}_]|$)");

pub fn check(repo: &Repo, fid: &str, st: &State) -> Result<Vec<String>> {
    let d = repo.fdir(fid);
    let mut problems = Vec::new();
    let approval = |what: &str, kind: Phase| {
        let cmd = flow_cmd(&repo.root);
        format!("{what} the user's approval: `! {cmd} approve {kind}`")
    };
    match st.phase {
        Phase::Explore | Phase::Spec => {
            let spec = st.phase == Phase::Spec;
            let name = if spec { "spec.md" } else { "research.md" };
            let required = repo.required_sections(name)?;
            let doc = Doc::load(&d.join(name))?;
            problems.extend(check_doc(doc.as_ref(), &required, name));
            let open = doc.map(|doc| unchecked_boxes(&doc.sections, "open questions", name));
            problems.extend(open.flatten());
            if spec && problems.is_empty() && !repo.approval_valid(fid, st, Phase::Spec)? {
                problems.push(approval("spec.md needs", Phase::Spec));
            }
        }
        Phase::Tickets => {
            let files = repo.tickets(fid);
            if files.is_empty() {
                problems.push("tickets/: no tickets yet".into());
            }
            let required = repo.required_sections("ticket.md")?;
            for t in &files {
                let label = format!("tickets/{}", file_name(t));
                let doc = Doc::load(t)?;
                problems.extend(check_doc(doc.as_ref(), &required, &label));
                if doc.is_some_and(|doc| gate_commands(&doc.sections).is_empty()) {
                    let why = "'## Verification steps' has no commands in a ``` block";
                    problems.push(format!("{label}: {why}"));
                }
            }
            if problems.is_empty() && !repo.approval_valid(fid, st, Phase::Tickets)? {
                problems.push(approval("tickets need", Phase::Tickets));
            }
        }
        Phase::Build => {
            if !repo.gates_current(st)? {
                problems.push("gates have not passed on the current code: run `flow gates`".into());
            }
        }
        Phase::Review => {
            let required = repo.required_sections("review.md")?;
            let review = Doc::load(&d.join("review.md"))?;
            problems.extend(check_doc(review.as_ref(), &required, "review.md"));
            let verdict = review.as_ref().map_or("", |r| r.sections.get("verdict"));
            if !VERDICT_PASS.is_match(verdict) {
                problems.push("review.md: '## Verdict' does not start with 'pass'".into());
            }
            if !repo.gates_current(st)? {
                let why = "code changed since the gates passed: `flow back`, fix, `flow gates`";
                problems.push(why.into());
            }
        }
        Phase::Ship => problems.extend(ship_problems(repo)?),
        Phase::Done | Phase::Abandoned => {}
    }
    Ok(problems)
}

fn ship_problems(repo: &Repo) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let work = format!("{}/", repo.work_rel);
    let mut dirty = repo.changed_paths()?;
    dirty.retain(|p| !p.starts_with(&work));
    if !dirty.is_empty() {
        let shown = dirty[..dirty.len().min(5)].join(", ");
        problems.push(format!("uncommitted changes: {shown}"));
    }
    let Some(deploy) = &repo.config.deploy else {
        return Ok(problems);
    };
    let branch = deploy.branch.as_deref().unwrap_or("main");
    let git_cmd = |args: &[&str]| {
        let mut c = Command::new("git");
        c.args(args).current_dir(&repo.root);
        c
    };
    // A fetch that fails shows as HEAD not being on the remote branch.
    let _ = git_cmd(&["fetch", "-q", "origin", branch]).stdin(Stdio::null()).output();
    let head = git(&repo.root, &["rev-parse", "HEAD"])?.trim().to_string();
    let remote = format!("origin/{branch}");
    if !git_cmd(&["merge-base", "--is-ancestor", &head, &remote]).status()?.success() {
        problems.push(format!("HEAD is not on {remote} yet: push it"));
        return Ok(problems);
    }
    let runs = match deploy::runs(deploy, &repo.root, 30) {
        Ok(runs) => runs,
        Err(e) => {
            problems.push(e.to_string());
            return Ok(problems);
        }
    };
    let mine: Vec<_> = runs.iter().filter(|r| r.head_sha == head).collect();
    if mine.is_empty() {
        let short: String = head.chars().take(7).collect();
        problems.push(format!("no CI jobs for {short} yet"));
    } else if mine.iter().any(|r| r.in_flight()) {
        problems.push(format!("deploy still running: {}", run_list(mine)));
    } else if !mine.iter().all(|r| r.passed()) {
        problems.push(format!("CI failed: {}", run_list(mine)));
    }
    if let Some(status) = deploy::health(deploy).filter(|s| *s != 200) {
        problems.push(format!("health check returned {status}"));
    }
    Ok(problems)
}
