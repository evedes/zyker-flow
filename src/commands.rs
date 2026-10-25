//! The CLI commands. Each returns the exit code, or an error printed as `flow: ...`.

use crate::checks::{check, stale_approvals};
use crate::config::{
    default_work_dir, now, read_json, write_json, Approval, Delegation, Event, Extra, GateResult,
    Gates, Judgement, Phase, State,
};
use crate::deploy::{self, exit_code};
use crate::error::{bail, Error, Result};
use crate::judge::{self, Verdict, GATES, MAX_REJECTS};
use crate::markdown::{gate_commands, Sections};
use crate::paths::{file_name, flow_cmd, io_error, write};
use crate::repo::{git, Repo};
use crate::shell::USER_ONLY;
use crate::specs;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bullets(problems: &[String]) -> String {
    let lines: Vec<String> = problems.iter().map(|p| format!("- {p}")).collect();
    lines.join("\n")
}

/// Writes a template into the feature, unless the file is already there.
fn scaffold(repo: &Repo, fid: &str, st: &State, name: &str, dest: Option<PathBuf>) -> Result<()> {
    let dest = dest.unwrap_or_else(|| repo.fdir(fid).join(name));
    if dest.exists() {
        return Ok(());
    }
    let template = repo.template(name)?;
    let mut text = template.replace("{title}", &st.title).replace("{id}", fid);
    text = text.replace("{work_dir}", &repo.work_rel);
    if text.contains("{fingerprint}") {
        let fp = st.gates().map_or("?", |g| g.fingerprint.as_str());
        text = text.replace("{fingerprint}", fp);
    }
    write(&dest, text)
}

/// `Fix login!` gives `fix-login`.
fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut gap = false;
    for c in text.to_lowercase().chars() {
        let keep = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if keep || !gap {
            slug.push(if keep { c } else { '-' });
        }
        gap = !keep;
    }
    slug.trim_matches('-').to_string()
}

/// The number of a feature directory (`007-x` is 7).
fn feature_number(name: &str) -> Option<u32> {
    let (num, rest) = name.split_at_checked(3)?;
    let ok = num.bytes().all(|b| b.is_ascii_digit()) && rest.starts_with('-');
    ok.then(|| num.parse().ok()).flatten()
}

pub fn new(repo: &Repo, args: &[String]) -> Result<i32> {
    let Some(name) = args.first() else {
        bail!("usage: flow new <slug> [\"<title>\"]");
    };
    let slug = slugify(name);
    // flow numbers features itself.
    let slug = match feature_number(&slug) {
        Some(_) => slug[4..].to_string(),
        None => slug,
    };
    if slug.is_empty() {
        bail!("slug {name:?} has no letters or digits to name the feature by");
    }
    let title = args.get(1).cloned().unwrap_or_else(|| {
        let words = slug.replace('-', " ");
        let mut chars = words.chars();
        chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
    });
    if let Some(active) = repo.load_global()?.active() {
        let cmd = flow_cmd(&repo.root);
        bail!("feature {active} is still active; finish it or `! {cmd} abandon`");
    }
    if let Some(why) = specs::bootstrap_problem(repo)? {
        bail!("{why}");
    }
    let entries = fs::read_dir(&repo.work).into_iter().flatten().flatten();
    let last = entries.filter_map(|e| feature_number(e.file_name().to_str()?)).max();
    let fid = format!("{:03}-{slug}", last.unwrap_or(0) + 1);
    let st = State {
        id: fid.clone(),
        title,
        phase: Phase::Explore,
        approvals: Some(Default::default()),
        gates: Some(None),
        history: vec![Event::now(Phase::Explore)],
        reviewed_fingerprint: None,
        delegated: None,
        judgements: None,
        extra: Extra::new(),
    };
    repo.save_state(&fid, &st)?;
    scaffold(repo, &fid, &st, "research.md", None)?;
    repo.set_active(Some(&fid))?;
    println!("started {fid} in phase explore; fill in {}/{fid}/research.md", repo.work_rel);
    Ok(0)
}

pub fn status(repo: &Repo) -> Result<i32> {
    if repo.load_global()?.paused() {
        let cmd = flow_cmd(&repo.root);
        println!("flow is PAUSED: hooks allow everything until `! {cmd} resume`");
    }
    let Some((fid, st)) = repo.active()? else {
        println!("no active feature");
        return Ok(0);
    };
    println!("{fid} — {}", st.title);
    let phase = |p: Phase| {
        if p == st.phase {
            format!("[{p}]")
        } else {
            p.to_string()
        }
    };
    println!("phase: {}", Phase::ALL.map(phase).join(" > "));
    for kind in GATES {
        if let Some(a) = st.approval(kind) {
            let stale = "STALE (artifact changed since approval)";
            let valid = repo.approval_valid(&fid, &st, kind)?;
            let by = if a.by.is_some() { " (by the judge)" } else { "" };
            println!("approval {kind}: {}{by}", if valid { "valid" } else { stale });
        }
    }
    let delegated: Vec<&str> =
        GATES.iter().filter(|g| st.delegated(**g).is_some()).map(|g| g.name()).collect();
    if !delegated.is_empty() {
        println!("delegated to the judge: {}", delegated.join(", "));
    }
    if let Some(j) = st.judgements().iter().rev().find(|j| j.gate == st.phase.name()) {
        println!("judge on {}: {} — {}", j.gate, j.verdict, j.summary);
    }
    if let Some(gates) = st.gates() {
        let result = if gates.passed { "passed" } else { "FAILED" };
        let stale = gates.passed && !repo.gates_current(&st)?;
        let note = if stale { " (stale: code changed since)" } else { "" };
        println!("gates: {result}{note}");
    }
    let problems = check(repo, &fid, &st, true)?;
    if problems.is_empty() {
        println!("ready: `flow advance`");
    } else {
        println!("to advance:");
        problems.iter().for_each(|p| println!("  - {p}"));
    }
    Ok(0)
}

pub fn check_cmd(repo: &Repo) -> Result<i32> {
    let (fid, st) = repo.require_active()?;
    let problems = check(repo, &fid, &st, true)?;
    problems.iter().for_each(|p| println!("- {p}"));
    if !problems.is_empty() {
        return Ok(1);
    }
    println!("{}: complete", st.phase);
    Ok(0)
}

pub fn advance(repo: &Repo) -> Result<i32> {
    let (fid, mut st) = repo.require_active()?;
    let problems = check(repo, &fid, &st, true)?;
    if !problems.is_empty() {
        bail!("cannot advance from {}:\n{}", st.phase, bullets(&problems));
    }
    let Some(next) = st.phase.next() else {
        bail!("cannot advance from {}", st.phase);
    };
    match next {
        Phase::Spec => scaffold(repo, &fid, &st, "spec.md", None)?,
        // Back from a later phase, the tickets are kept.
        Phase::Tickets if repo.tickets(&fid).is_empty() => {
            let first = repo.fdir(&fid).join("tickets").join("T01.md");
            scaffold(repo, &fid, &st, "ticket.md", Some(first))?;
        }
        Phase::Review => {
            // Each round reviews the code as it is now: an earlier verdict is kept as
            // review-<n>.md.
            let review = repo.fdir(&fid).join("review.md");
            if review.exists() {
                let kept = (1..).map(|n| repo.fdir(&fid).join(format!("review-{n}.md")));
                let kept = kept.into_iter().find(|p| !p.exists()).expect("a free name");
                fs::rename(&review, &kept).map_err(|e| io_error(&review, e))?;
            }
            scaffold(repo, &fid, &st, "review.md", None)?;
        }
        Phase::Ship => st.reviewed_fingerprint = Some(Some(repo.fingerprint()?)),
        Phase::Done => {
            let merge = specs::merge(repo, &fid)?;
            if !merge.problems.is_empty() {
                bail!("cannot merge the spec changes:\n{}", bullets(&merge.problems));
            }
            let written = specs::apply(repo, merge)?;
            if !written.is_empty() {
                println!("merged the spec changes into {}", written.join(", "));
            }
        }
        _ => {}
    }
    st.phase = next;
    st.history.push(Event::now(next));
    repo.save_state(&fid, &st)?;
    if next == Phase::Done {
        repo.set_active(None)?;
    }
    println!("{fid}: now in phase {next}");
    Ok(0)
}

pub fn back(repo: &Repo) -> Result<i32> {
    let (fid, mut st) = repo.require_active()?;
    let prev = match st.phase.prev() {
        Some(p) if st.phase != Phase::Done => p,
        _ => bail!("cannot go back from {}", st.phase),
    };
    st.phase = prev;
    st.history.push(Event { back: Some(true), ..Event::now(prev) });
    repo.save_state(&fid, &st)?;
    println!("{fid}: back to phase {prev}");
    Ok(0)
}

pub fn approve(repo: &Repo, args: &[String]) -> Result<i32> {
    let (fid, mut st) = repo.require_active()?;
    let kind = match args {
        [a] => GATES.into_iter().find(|g| g.name() == a),
        _ => None,
    };
    let Some(kind) = kind else {
        bail!("usage: flow approve spec|tickets|review");
    };
    if st.phase != kind {
        bail!("{kind} can only be approved in phase {kind} (now {})", st.phase);
    }
    let problems = check(repo, &fid, &st, false)?;
    if !problems.is_empty() {
        bail!("{kind} is not complete:\n{}", bullets(&problems));
    }
    let hash = repo.gate_hash(&fid, kind)?;
    let approval = Approval { hash, at: now(), ..Default::default() };
    let approvals = st.approvals.get_or_insert_with(Default::default);
    approvals.insert(kind.name().into(), approval);
    repo.save_state(&fid, &st)?;
    let until = if kind == Phase::Review { "the code" } else { "the file" };
    println!("{kind} approved; it stays approved until {until} changes");
    Ok(0)
}

pub fn delegate(repo: &Repo, args: &[String]) -> Result<i32> {
    let (fid, mut st) = repo.require_active()?;
    let usage = "usage: flow delegate spec|tickets|review... (or off)";
    if let [off] = args {
        if off == "off" {
            st.delegated = None;
            repo.save_state(&fid, &st)?;
            println!("{fid}: nothing is delegated; spec, tickets and review need the user");
            return Ok(0);
        }
    }
    let gates: Option<Vec<Phase>> =
        args.iter().map(|a| GATES.into_iter().find(|g| g.name() == a)).collect();
    let Some(gates) = gates.filter(|g| !g.is_empty()) else {
        bail!("{usage}");
    };
    let (at, judgements) = (now(), st.judgements().len());
    let delegated = st.delegated.get_or_insert_with(Default::default);
    for g in &gates {
        let delegation = Delegation { at: at.clone(), judgements, ..Default::default() };
        delegated.insert(g.name().into(), delegation);
    }
    repo.save_state(&fid, &st)?;
    let names: Vec<&str> = gates.iter().map(|g| g.name()).collect();
    println!(
        "{fid}: the judge may pass {} (`flow judge`), until the feature is done or abandoned, \
         or `flow delegate off`",
        names.join(", ")
    );
    Ok(0)
}

/// Prints a judgement, as `flow judge` gives it.
fn report(j: &Judgement) {
    println!("judge: {} — {}", j.verdict, j.summary);
    j.findings.iter().for_each(|f| println!("  - {f}"));
    if !j.questions.is_empty() {
        println!("questions for the user:");
        j.questions.iter().for_each(|q| println!("  - {q}"));
    }
}

/// review.md as the judge's record of its verdict on the code.
fn review_record(st: &State, j: &Judgement) -> String {
    let verdict = match j.verdict.as_str() {
        "approve" => "pass",
        "reject" => "fail",
        _ => "undecided",
    };
    let list = |items: &[String]| match items {
        [] => "None.".to_string(),
        _ => items.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n"),
    };
    let mut text = format!(
        "# Review: {}\n\n## Verdict\n{verdict} — {}\n\n## Findings\n{}\n",
        st.title,
        j.summary,
        list(&j.findings)
    );
    if !j.questions.is_empty() {
        text += &format!("\n## Questions for the user\n{}\n", list(&j.questions));
    }
    text + &format!("\nJudged by `flow judge` on fingerprint {} at {}.\n", j.hash, j.at)
}

pub fn judge(repo: &Repo) -> Result<i32> {
    let (fid, st) = repo.require_active()?;
    let (gate, cmd) = (st.phase, flow_cmd(&repo.root));
    if !GATES.contains(&gate) {
        bail!("the judge passes spec, tickets and review (now {gate})");
    }
    let Some(since) = st.delegated(gate).map(|d| d.judgements) else {
        bail!(
            "the user has not delegated {gate} to the judge: they approve it \
             (`! {cmd} approve {gate}`), or delegate it (`! {cmd} delegate {gate}`)"
        );
    };
    let problems = check(repo, &fid, &st, false)?;
    if !problems.is_empty() {
        bail!("{gate} is not ready to judge:\n{}", bullets(&problems));
    }
    let hash = repo.gate_hash(&fid, gate)?.unwrap_or_default();
    let mine = |j: &&Judgement| j.gate == gate.name();
    if let Some(j) = st.judgements().iter().filter(mine).find(|j| j.hash == hash) {
        println!("the judge has judged this version of {gate} already");
        report(j);
        return Ok(if j.verdict == "approve" { 0 } else { 1 });
    }
    let since_delegated = st.judgements().iter().skip(since).filter(mine);
    let rejects = since_delegated.filter(|j| j.verdict == "reject").count();
    if rejects >= MAX_REJECTS {
        bail!(
            "the judge rejected {gate} {rejects} times since it was delegated: the user \
             approves it (`! {cmd} approve {gate}`), or delegates it again for \
             {MAX_REJECTS} more tries (`! {cmd} delegate {gate}`)"
        );
    }
    println!("judging {gate} of {fid} with a read-only `claude -p`; this takes minutes");
    let Verdict { verdict, summary, findings, questions } =
        judge::ask(repo, &judge::prompt(repo, &fid, &st, gate)?)?;
    if repo.gate_hash(&fid, gate)?.unwrap_or_default() != hash {
        bail!("{gate} changed while the judge read it; nothing recorded: `{cmd} judge` again");
    }
    // The state is read again: the user may have acted meanwhile.
    let (_, mut st) = repo.require_active()?;
    let at = now();
    let j = Judgement {
        gate: gate.name().into(),
        hash: hash.clone(),
        verdict,
        summary,
        findings,
        questions,
        at: at.clone(),
        ..Default::default()
    };
    if gate == Phase::Review {
        write(&repo.fdir(&fid).join("review.md"), review_record(&st, &j))?;
    }
    let approved = j.verdict == "approve";
    if approved {
        let by = Some("judge".to_string());
        let approval = Approval { hash: Some(hash), at, by, ..Default::default() };
        st.approvals.get_or_insert_with(Default::default).insert(gate.name().into(), approval);
    }
    report(&j);
    st.judgements.get_or_insert_with(Vec::new).push(j);
    repo.save_state(&fid, &st)?;
    Ok(if approved { 0 } else { 1 })
}

pub fn gates(repo: &Repo) -> Result<i32> {
    let (fid, mut st) = repo.require_active()?;
    // Not in review: rerunning them there would let code the review never saw reach ship.
    if st.phase != Phase::Build {
        bail!("gates run in build (now {})", st.phase);
    }
    let stale = stale_approvals(repo, &fid, &st)?;
    if !stale.is_empty() {
        bail!("gates run only on the approved spec and tickets:\n{}", bullets(&stale));
    }
    let standing = repo.config.standing_gates.iter();
    let mut cmds: Vec<(String, String)> =
        standing.map(|c| ("standing".into(), c.clone())).collect();
    for t in repo.tickets(&fid) {
        let stem = t.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        cmds.extend(gate_commands(&Sections::read(&t)?).into_iter().map(|c| (stem.clone(), c)));
    }
    let mut results = Vec::new();
    for (source, command) in cmds {
        println!("\n== [{source}] {command}");
        let sh = Command::new("/bin/sh").arg("-c").arg(&command).current_dir(&repo.root).status();
        let exit = exit_code(sh.map_err(|e| Error(format!("/bin/sh: {e}")))?);
        if exit != 0 {
            println!("== FAILED (exit {exit})");
        }
        results.push(GateResult { source, command, exit, ..Default::default() });
    }
    let passed = results.iter().all(|r| r.exit == 0);
    let count = results.len();
    let (fingerprint, at) = (repo.fingerprint()?, now());
    st.gates = Some(Some(Gates { passed, fingerprint, at, results, ..Default::default() }));
    repo.save_state(&fid, &st)?;
    println!("\ngates {} ({count} command(s))", if passed { "passed" } else { "FAILED" });
    Ok(if passed { 0 } else { 1 })
}

pub fn deploy(repo: &Repo) -> Result<i32> {
    let Some(deploy) = &repo.config.deploy else {
        bail!("no deploy config in .flow.json");
    };
    let head = git(&repo.root, &["rev-parse", "HEAD"])?;
    for t in deploy::runs(deploy, &repo.root, 15)? {
        let mark = if t.head_sha == head.trim() { "*" } else { " " };
        let sha: String = t.head_sha.chars().take(7).collect();
        println!("{mark} {} {:<14} {:<9} {sha}", t.id(), t.name, t.status);
    }
    let health = deploy::health(deploy);
    let health = health.map_or("None".into(), |s| s.to_string());
    println!("health: {health}");
    Ok(0)
}

pub fn pause(repo: &Repo, paused: bool) -> Result<i32> {
    let mut g = repo.load_global()?;
    g.paused = Some(paused);
    repo.save_global(&g)?;
    let done = "flow paused: hooks allow everything";
    println!("{}", if paused { done } else { "flow resumed" });
    Ok(0)
}

pub fn abandon(repo: &Repo) -> Result<i32> {
    let g = repo.load_global()?;
    let Some(fid) = g.active() else {
        bail!("no active feature");
    };
    // The state is gone if `git clean` removed the feature; the active slot is cleared anyway.
    let path = repo.fdir(fid).join("state.json");
    if path.is_file() {
        let mut st: State = read_json(&path)?;
        st.phase = Phase::Abandoned;
        st.history.push(Event::now(Phase::Abandoned));
        repo.save_state(fid, &st)?;
    }
    let fid = fid.to_string();
    repo.set_active(None)?;
    println!("{fid} abandoned");
    Ok(0)
}

/// A starting point for `flow init`, keyed by the file that marks the stack: protected
/// paths, standing gates and codegen patterns. They are guesses the repo edits.
type Stack =
    (&'static str, &'static [&'static str], &'static [&'static str], &'static [&'static str]);

#[rustfmt::skip]
const STACKS: [Stack; 4] = [
    ("mix.exs", &["lib/", "test/", "config/", "priv/", "assets/", "mix.exs", "mix.lock"],
     &["mix compile --warnings-as-errors", "mix format --check-formatted", "mix test"],
     &[r"\bmix\s+(phx|ecto)\.gen\.\S", r"\bmix\s+igniter\.\S", r"\bmix\s+format\b(?!.*--check-formatted)"]),
    ("Cargo.toml", &["src/", "tests/", "Cargo.toml", "Cargo.lock"],
     &["cargo fmt --check", "cargo clippy -- -D warnings", "cargo test"],
     &[r"\bcargo\s+fmt\b(?!.*--check)"]),
    ("pyproject.toml", &["src/", "tests/", "pyproject.toml"],
     &["python -m pytest"],
     &[r"\bruff\s+format\b(?!.*--check)", r"\bblack\b(?!.*--check)"]),
    ("package.json", &["src/", "test/", "package.json", "package-lock.json"],
     &["npm test"],
     &[r"\bprettier\b.*--write", r"\beslint\b.*--fix"]),
];

/// Permission rules backing up the hook: the user-only commands and the state files.
/// Edit(path) rules cover every file-editing tool; Claude Code ignores Write(path) ones.
fn deny_rules(work_rel: &str) -> impl Iterator<Item = String> + '_ {
    let commands = USER_ONLY.iter().map(|c| format!("Bash(flow {c}:*)"));
    commands.chain(["flow.json", "*/state.json"].map(|p| format!("Edit({work_rel}/{p})")))
}

/// settings.permissions.deny, created when missing.
fn deny_list(settings: &mut Value) -> Option<&mut Vec<Value>> {
    let permissions = settings.as_object_mut()?.entry("permissions").or_insert_with(|| json!({}));
    permissions.as_object_mut()?.entry("deny").or_insert_with(|| json!([])).as_array_mut()
}

pub fn init(root: &Path) -> Result<i32> {
    if root.join(".flow.json").exists() {
        bail!(".flow.json already exists");
    }
    let settings_path = root.join(".claude").join("settings.json");
    let mut settings = if settings_path.is_file() {
        let not_json =
            |_| Error(format!("{} is not plain JSON; nothing written", settings_path.display()));
        read_json(&settings_path).map_err(not_json)?
    } else {
        json!({})
    };
    let Some(deny) = deny_list(&mut settings) else {
        bail!("{} has no permissions.deny list; nothing written", settings_path.display());
    };
    let (marker, protected, gates, codegen) =
        STACKS.into_iter().find(|s| root.join(s.0).is_file()).unwrap_or(("", &["src/"], &[], &[]));
    let config = json!({
        "work_dir": default_work_dir(), "protected_paths": protected, "standing_gates": gates, "codegen": codegen,
    });
    write_json(&root.join(".flow.json"), &config)?;
    let repo = Repo::open(root)?;
    let standing = repo.work.join("STANDING.md");
    if !standing.exists() {
        let template = repo.template("STANDING.md")?;
        let text = template.replace("{project}", &file_name(root));
        write(&standing, text.replace("{work_dir}", &repo.work_rel))?;
    }
    let missing: Vec<String> = deny_rules(&repo.work_rel)
        .filter(|r| !deny.iter().any(|d| d.as_str() == Some(r)))
        .collect();
    deny.extend(missing.into_iter().map(Value::String));
    write_json(&settings_path, &settings)?;
    let (work, cmd) = (&repo.work_rel, flow_cmd(root));
    let how = if marker.is_empty() {
        "no known stack: guessed".into()
    } else {
        format!("defaults for {marker}")
    };
    println!(
        "wrote .flow.json ({how}), {work}/STANDING.md and deny rules in .claude/settings.json"
    );
    println!(
        "next: check protected_paths, standing_gates and codegen in .flow.json, fill in \
         {work}/STANDING.md, and add a deploy section if ship should wait for CI; if the \
         repo already has code, write its living spec in {work}/specs/ before the first \
         feature (`{cmd} new` refuses until then)"
    );
    Ok(0)
}
