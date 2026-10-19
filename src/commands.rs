//! The CLI commands. Each returns the exit code, or an error printed as `flow: ...`.

use crate::checks::{check, stale_approvals};
use crate::config::{
    now, read_json, write_json, Approval, Event, Extra, GateResult, Gates, Phase, State,
};
use crate::deploy::{self, exit_code};
use crate::error::{bail, Error, Result};
use crate::markdown::{gate_commands, Sections};
use crate::paths::{file_name, flow_cmd, io_error, write};
use crate::repo::{git, Repo};
use crate::shell::USER_ONLY;
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
    for kind in Phase::APPROVABLE {
        if st.approval(kind).is_some() {
            let stale = "STALE (artifact changed since approval)";
            let valid = repo.approval_valid(&fid, &st, kind)?;
            println!("approval {kind}: {}", if valid { "valid" } else { stale });
        }
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
        [a] if a == "spec" => Phase::Spec,
        [a] if a == "tickets" => Phase::Tickets,
        _ => bail!("usage: flow approve spec|tickets"),
    };
    if st.phase != kind {
        bail!("{kind} can only be approved in phase {kind} (now {})", st.phase);
    }
    let problems = check(repo, &fid, &st, false)?;
    if !problems.is_empty() {
        bail!("{kind} is not complete:\n{}", bullets(&problems));
    }
    let hash = repo.artifact_hash(&fid, kind)?;
    let approval = Approval { hash, at: now(), ..Default::default() };
    let approvals = st.approvals.get_or_insert_with(Default::default);
    approvals.insert(kind.name().into(), approval);
    repo.save_state(&fid, &st)?;
    println!("{kind} approved; it stays approved until the file changes");
    Ok(0)
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
        "work_dir": "work", "protected_paths": protected, "standing_gates": gates, "codegen": codegen,
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
    let work = &repo.work_rel;
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
         {work}/STANDING.md, and add a deploy section if ship should wait for CI"
    );
    Ok(0)
}
